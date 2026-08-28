// @vitest-environment jsdom
// QURATOR-135 — the presence pill has no "unknown" state, so not-yet-polled renders as a definite
// Offline. A contact who never logged off showed "Offline" for ~90s after launch: the contacts
// refresh is gated by REFRESH_FRESHNESS_MS = 10 min while the online poll runs every 20s, and the
// pill's binary `{#if peer.online} Online {:else} Offline {/if}` had nowhere to put "no data yet".
// This is the repo's recurring confident-negative shape (QURATOR-67/68/83): absence-of-data
// presented as positive knowledge of a negative.
//
// BEHAVIOURAL mount tests (not a source-scan — per the repo rule, source-scans are the documented
// origin of vacuous controls): the Contacts page is mounted with only `$lib/api.js` mocked, the
// roster seeded straight into the `contacts` store, and the assertions target the RENDERED PILL
// text/class.
//
// ⚠ jsdom computes no layout: these tests prove the right TEXT and CLASS are chosen, not that the
// pill renders as a visually distinct colour/shape. Nothing here pins the dashed-border/italic
// styling — that is only checkable in a real browser (the Playwright + getComputedStyle recipe).
// A suite that pins ATTRIBUTES is blind to a change that only alters SHAPE.
//
// MUTATION PROOF (per §9 — a green test proves nothing until seen red): test 1 was run against the
// UNFIXED +page.svelte before the fix landed and reds exactly as expected — the binary pill
// rendered "Offline" for a contact with no presence data. Red output preserved in the task report.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/svelte';
import ContactsPage from './+page.svelte';
import { onlineCount } from '$lib/api.js';
import { contacts, contactsLoadError } from '$lib/stores.js';
import type { CachedPeer, Profile } from '$lib/types.js';

// The api mock — every Tauri command Contacts imports is stubbed. onlineCount resolves with no
// `fresh` set (the pre-poll state: the poll has not answered yet), getContacts with an empty list
// (the roster is seeded directly into the store, as the layout would have done).
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
	getContacts: vi.fn().mockResolvedValue([]),
	privateAudienceList: vi.fn().mockResolvedValue([]),
	privateAudienceSet: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

const PROF: Profile = {
	display_name: 'Q135 Peer',
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
};

function peer(overrides: Partial<CachedPeer>): CachedPeer {
	return {
		npub: 'npub1q135' + 'a'.repeat(50),
		collections: [],
		online: false,
		last_fetched: new Date().toISOString(),
		local_tags: [],
		profile: PROF,
		...overrides,
	};
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	contactsLoadError.set(false);
});

describe('QURATOR-135 — unknown presence is not Offline', () => {
	it('no beacon observed yet → the unknown pill renders and the word "Offline" does NOT', async () => {
		contacts.set([peer({ last_presence: undefined })]);
		const { container, queryAllByText } = render(ContactsPage);
		await waitFor(() => expect(container.querySelectorAll('.pill-unknown').length).toBe(1));
		// THE core assertion: absence-of-data must not render as a definite Offline.
		expect(queryAllByText('Offline').length).toBe(0);
	});

	it('a beacon outside the window → Offline', async () => {
		contacts.set([peer({ last_presence: new Date(Date.now() - 3_600_000).toISOString() })]);
		const { container } = render(ContactsPage);
		await waitFor(() => expect(container.querySelectorAll('.pill-offline').length).toBe(1));
		expect(container.querySelectorAll('.pill-unknown').length).toBe(0);
	});

	it('a live beacon → the Online pill', async () => {
		contacts.set([peer({ last_presence: new Date().toISOString() })]);
		const { container } = render(ContactsPage);
		// The contacts-page Online pill is dot-only (no text), so it is asserted by class.
		await waitFor(() => expect(container.querySelectorAll('.pill-online').length).toBe(1));
		expect(container.querySelectorAll('.pill-unknown').length).toBe(0);
	});
});

// QURATOR-135, the OWED second half — "first poll fires on mount, not on the first
// REFRESH_INTERVAL/ONLINE_POLL_VISIBLE_MS tick."
//
// Why this is the acquisition half: `online_count` is a cached read — the first call CLAIMS the
// refresh slot and returns the (empty) cache immediately, the spawned relay query lands later, and
// the SECOND read is what picks up the completed pills. If the page's first read only happens on
// the first interval tick, the whole relay query is shifted a full ONLINE_POLL_VISIBLE_MS (20 s)
// later than it needs to be, stacking with the backend's first-connect latency into the owner's
// ~90 s cold start. Firing the read in onMount means the relay query is claimed the moment the
// page exists.
//
// No fake timers and no timer advancement: onMount flushes during render under testing-library, so
// reaching for the tick is provably unnecessary — if the poll only ran on the interval, this test
// would time out waiting (a real 20 s interval never fires inside a 5 s waitFor).
describe('QURATOR-135 — the first poll fires on mount, not on the first tick', () => {
	it('online_count has been called by the time the mount settles — no tick fired', async () => {
		contacts.set([peer({ last_presence: undefined })]);
		render(ContactsPage);
		await waitFor(() => expect(vi.mocked(onlineCount).mock.calls.length).toBeGreaterThanOrEqual(1));
	});
});
