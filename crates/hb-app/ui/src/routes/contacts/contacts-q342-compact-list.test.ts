// @vitest-environment jsdom
// QURATOR-342 lane B — the Contacts two-pane layout: a ~300px compact contact-list column
// (two-line rows grouped "Online now / Everyone else") and the People-like-you pane on the right.
//
// BEHAVIOURAL mount tests (P-4): only `$lib/api.js` + `$app/navigation` are mocked, the roster is
// seeded straight into the `contacts` store, and the assertions run against the RENDERED page.
//
// MUTATION PROOFS (§9 / P-10) — every test below was SEEN RED against the exact production edit
// named in its comment, with the file restored (cp backup + `touch`) between proofs:
//  • row-click navigation   ← `/browse?peer=` mutated to `/chat?peer=` in the card onclick → REDS.
//  • rail/tag-filter absence ← the removed "Filter by tag" row + `allTags` state restored VERBATIM → REDS.
//  • PeopleLikeYou pane     ← `<PeopleLikeYou />` deleted from the template → REDS.
//  • ask-access absence     ← the removed "Ask for access" button restored VERBATIM → REDS this test
//                             AND ask-access-w2.test.ts's updated absence pin.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import ContactsPage from './+page.svelte';
import { goto } from '$app/navigation';
import { contacts } from '$lib/stores.js';
import type { ContactSummary, Collection, Profile } from '$lib/types.js';

// jsdom has no ResizeObserver, and `bioMeasure` (the M23 W6 bio-overflow action) constructs one the
// moment a contact HAS a bio (same stub as contacts-chevron-item3.test.ts — a jsdom gap, not a
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
	// QURATOR-342 — the People-like-you panel self-fetches its ranking (lane C's contract).
	similarPeople: vi.fn().mockResolvedValue({ people: [], cold_start: false }),
	topicDiscoverPaint: vi.fn().mockResolvedValue([]),
}));

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

const NPUB_A = 'npub1q342a' + 'a'.repeat(50);
const NPUB_B = 'npub1q342b' + 'a'.repeat(50);

const BASE_PROFILE: Profile = {
	display_name: 'Somebody Hoardish',
	bio: 'keeps films and zines',
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-09-01T00:00:00Z',
};

function peer(overrides: Partial<ContactSummary> & { npub: string }): ContactSummary {
	return {
		has_browse_key: true,
		collections: [],
		online: false,
		last_fetched: new Date().toISOString(),
		local_tags: [],
		profile: BASE_PROFILE,
		...overrides,
	};
}

function pubCollection(slug: string): Collection {
	return {
		slug,
		path_alias: slug,
		description: '',
		item_count: 12,
		est_size: '12.0 TB',
		total_bytes: 12_000_000_000_000,
		content_types: [],
		tags: [],
		languages: [],
		visibility: 'Public',
		last_updated: '2026-09-01T00:00:00Z',
		listing: [],
	};
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

describe('QURATOR-342 lane B — row click opens Browse', () => {
	it('a plain click on the row navigates to /browse?peer=<npub>; inner controls and modifier clicks do not', async () => {
		contacts.set([
			peer({ npub: NPUB_A, online: true }),
			peer({ npub: NPUB_B }),
		]);
		const { container } = render(ContactsPage);
		const rows = await waitFor(() => {
			const found = container.querySelectorAll('.contact-card');
			expect(found.length).toBe(2);
			return found;
		});
		// Online bucket renders first, so rows[1] is the offline peer NPUB_B in "Everyone else".
		// The pin: the plain click carries the npub to Browse, encodeURIComponent'd.
		fireEvent.click(rows[1]);
		await waitFor(() => expect(vi.mocked(goto)).toHaveBeenCalledWith('/browse?peer=' + NPUB_B));
		// A click on an inner control (⋯) keeps that control's own action — no navigation.
		fireEvent.click(rows[1].querySelector('.row-menu-btn')!);
		// A MODIFIER click keeps selection semantics — no navigation.
		await fireEvent.click(rows[1], { shiftKey: true });
		expect(vi.mocked(goto)).toHaveBeenCalledTimes(1);
	});
	// MUTATION PROOF: with `/browse?peer=` mutated to `/chat?peer=` in the card onclick this test
	// REDS (goto called with '/chat?peer=…' instead of '/browse?peer=…'); restored + touch → green.

	it('a double-click still opens Chat — the single-click Browse must not pre-empt it', async () => {
		// Review finding (orchestrator, 2026-09-28): the first click of a double-click fires `click`
		// too, so an immediate goto('/browse…') navigated away before `dblclick` → Chat could land.
		// P-10 MUTATION: in +page.svelte delete `cancelRowBrowse(); ` from the card's ondblclick —
		// the held Browse then fires after the Chat goto and this test REDS on the /browse call.
		contacts.set([peer({ npub: NPUB_B })]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => {
			const r = container.querySelector('.contact-card');
			expect(r).toBeTruthy();
			return r!;
		});
		vi.mocked(goto).mockClear();
		fireEvent.click(row);
		fireEvent.dblClick(row);
		await new Promise((r) => setTimeout(r, 400));
		expect(vi.mocked(goto)).toHaveBeenCalledWith('/chat?peer=' + NPUB_B);
		expect(vi.mocked(goto)).not.toHaveBeenCalledWith('/browse?peer=' + NPUB_B);
	});
});

describe('QURATOR-342 lane B — the removed controls stay removed', () => {
	it('no A-Z rail and no Filter-by-tag row render (search covers tags)', async () => {
		// The fixture carries a local tag so the OLD page's `{#if allTags.length > 0}` branch would
		// have rendered the collapsible "Filter by tag" row — the absence assert is not vacuous.
		contacts.set([peer({ npub: NPUB_A, local_tags: ['zz-lonely-tag'] })]);
		const { container } = render(ContactsPage);
		await waitFor(() => expect(container.querySelector('.contact-card')).toBeTruthy());
		expect(container.querySelector('.az-rail')).toBeNull();
		expect(container.querySelector('.tagfilter-row')).toBeNull();
		expect(container.querySelector('.tag-filter-row')).toBeNull();
	});
	// MUTATION PROOF: with the removed "Filter by tag" row + `allTags`/`filterTag`/`tagFilterOpen`
	// state restored VERBATIM, this test REDS (the row renders again); restored + touch → green.
});

describe('QURATOR-342 lane B — the right pane is People-like-you', () => {
	it('PeopleLikeYou is mounted in the people pane', async () => {
		contacts.set([peer({ npub: NPUB_A })]);
		const { container } = render(ContactsPage);
		await waitFor(() => expect(container.querySelector('.contact-card')).toBeTruthy());
		expect(container.querySelector('.people-pane')).not.toBeNull();
		// Source pins: the component is imported from lane C's file and actually rendered.
		// (process.cwd() + resolve, not import.meta.url — under jsdom that is an http:// URL and
		// readFileSync rejects it; same idiom as contacts-m22-drag.test.ts.)
		const src = readFileSync(resolve(process.cwd(), 'src/routes/contacts/+page.svelte'), 'utf8');
		expect(src).toMatch(/import PeopleLikeYou from '\$lib\/components\/PeopleLikeYou\.svelte'/);
		expect(src).toMatch(/<PeopleLikeYou \/>/);
	});
	// MUTATION PROOF: with `<PeopleLikeYou />` deleted from the template this test REDS
	// (`<PeopleLikeYou />` no longer in the source); restored + touch → green.
});

describe('QURATOR-342 lane B — no ask-for-access on a locked contact', () => {
	it('a locked contact renders with the lock overlay and NO Ask-for-access control', async () => {
		contacts.set([peer({ npub: NPUB_A, has_browse_key: false, collections: [pubCollection('films')] })]);
		const { container } = render(ContactsPage);
		await waitFor(() => expect(container.querySelector('.contact-card')).toBeTruthy());
		// The fixture is REALLY locked (badge.locked drives the overlay) — the absence below is not
		// a fixture mistake.
		expect(container.querySelector('.lock-overlay')).not.toBeNull();
		expect(container.querySelector('.ask-access-btn')).toBeNull();
		expect([...container.querySelectorAll('button, a')].some((el) => (el.textContent ?? '').includes('Ask for access'))).toBe(false);
	});
	// MUTATION PROOF: with the removed "Ask for access" button restored VERBATIM, this test REDS
	// (the button renders again) AND ask-access-w2.test.ts's updated absence pin REDS alongside it;
	// restored + touch → green.
});
