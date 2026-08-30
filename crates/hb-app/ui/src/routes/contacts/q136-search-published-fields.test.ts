// @vitest-environment jsdom
// QURATOR-136 — "in contacts, the search button only searches name and bio. Not the other fields
// like tags." The actual cause was one level finer: search already covered YOUR OWN local tags
// (`peer.local_tags`) but omitted the contact's PUBLISHED profile fields, including their own tags
// (`peer.profile.tags`) — so locally tagging someone made them findable while a tag they themselves
// published silently found nothing.
//
// BEHAVIOURAL mount tests (not a source-scan): the Contacts page is mounted with only `$lib/api.js`
// mocked, the roster seeded straight into the `contacts` store, the query typed into the real
// subheader search input, and the assertion is "does the row still render". Each fixture carries its
// test term in EXACTLY ONE field — display_name/bio/npub were already searchable, so a fixture whose
// tag string also appeared in the bio would pass on the broken code (vacuous).
//
// STRUCTURAL GUARD (the "hand-written list of sites cannot fix a hand-written list of sites" rule):
// the last describe derives the expected published-field haystack FROM THE PAGE'S OWN RENDERED
// CARD — it reads +page.svelte, collects every `p.<field>` the detail card template reads, and
// requires each one to be searched by matchesQuery (via a probe peer carrying a unique marker in
// every candidate field). A field added to the card without being added to search REDS, with no
// hand-maintained field list to drift. The deliberate exclusions (since/est_size/picture/updated)
// are stated as a literal list IN THE TEST so relaxing one is a visible decision, not a silent one.
//
// MUTATION PROOF (§9): every behavioural test below was run against the UNFIXED matchesQuery
// (display_name/petname/npub/bio/local_tags/content_types/collections only) BEFORE the fix landed
// and reds exactly as expected. Red output preserved in the task report.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import ContactsPage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import { matchesQuery } from '$lib/contacts-view.js';
import type { CachedPeer, Profile } from '$lib/types.js';

// jsdom has no ResizeObserver, and `bioMeasure` (the M23 W6 bio-overflow action) constructs one
// the moment a contact HAS a bio (same stub as contacts-chevron-item3.test.ts — a jsdom gap, not a
// production defect; jsdom computes no layout so `measure()` reads 0/0 and no clamp control shows).
class StubResizeObserver {
	observe() {}
	unobserve() {}
	disconnect() {}
}
vi.stubGlobal('ResizeObserver', StubResizeObserver);

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

const NPUB = 'npub1q136' + 'a'.repeat(50);

// A base profile with NOTHING matching any marker term — each test overrides exactly one field,
// so a pass can only come from that field being searched.
const BASE_PROFILE: Profile = {
	display_name: 'Unremarkable Person',
	bio: 'nothing distinctive here',
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
};

function peer(profile: Profile, npub = NPUB): CachedPeer {
	return {
		npub,
		collections: [],
		online: false,
		last_fetched: new Date().toISOString(),
		local_tags: [],
		profile,
	};
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

describe('QURATOR-136 — the contact\'s PUBLISHED fields are searchable', () => {
	// Each test overrides EXACTLY ONE field and asserts the row survives filtering. Red against
	// the unfixed haystack: these fields were all absent from it.
	for (const t of [
		{ name: 'their published profile tag (not your local tag)', term: 'zzq-laserdiscs', profile: { tags: ['zzq-laserdiscs'] } as Partial<Profile> },
		{ name: 'their published language', term: 'zzq-cantonese', profile: { languages: ['zzq-cantonese'] } as Partial<Profile> },
		{ name: 'their published willing_to chip', term: 'zzq-scanlate', profile: { willing_to: ['zzq-scanlate'] } as Partial<Profile> },
		{ name: 'their published region', term: 'zzq-osaka', profile: { location: 'zzq-osaka' } as Partial<Profile> },
		{ name: 'their published contact hint', term: 'zzq-matrix', profile: { contact_hint: 'zzq-matrix:zenith' } as Partial<Profile> },
	]) {
		it(`${t.name} finds the row`, async () => {
			contacts.set([peer({ ...BASE_PROFILE, ...t.profile })]);
			const { container } = render(ContactsPage);
			await waitFor(() => expect(container.querySelector('.contact-card')).toBeTruthy());
			const input = container.querySelector<HTMLInputElement>('.subheader-search input');
			expect(input).toBeTruthy();
			await fireEvent.input(input!, { target: { value: t.term } });
			await waitFor(() => expect(container.querySelector('.contact-card')).toBeTruthy());
		});
	}

	// The deliberate exclusion, pinned so relaxing it is a visible decision (see the doc comment on
	// matchesQuery): email is opted in for hand-copying from the card, not for search enumeration.
	it('their email is deliberately NOT searched (a pass here means the exclusion was relaxed)', () => {
		const p = peer({ ...BASE_PROFILE, email: 'zzq-quiet@example.net' });
		expect(matchesQuery(p, 'zzq-quiet')).toBe(false);
	});

	// The empty-state copy is the page's own honest-negative affordance — pin that it still works,
	// so a fixture bug can't turn "row hidden" into a silent pass.
	it('a term matching nothing still shows the no-match empty state', async () => {
		contacts.set([peer({ ...BASE_PROFILE })]);
		const { container, queryByText } = render(ContactsPage);
		await waitFor(() => expect(container.querySelector('.contact-card')).toBeTruthy());
		const input = container.querySelector<HTMLInputElement>('.subheader-search input');
		expect(input).toBeTruthy();
		await fireEvent.input(input!, { target: { value: 'zzz-no-such-term' } });
		await waitFor(() => expect(container.querySelector('.contact-card')).toBeNull());
		expect(queryByText(/No contacts match/)).toBeTruthy();
	});
});

// The structural guard (card-shape ↔ haystack agreement, derived from +page.svelte) lives in
// q136-structural-guard.test.ts — it needs node-env file reads, which jsdom forbids.
