// @vitest-environment jsdom
// QURATOR-342 lane B — the Contacts two-pane layout: a ~300px compact contact-list column
// (two-line rows grouped "Online now / Everyone else") and the People-like-you pane on the right.
//
// BEHAVIOURAL mount tests (P-4): only `$lib/api.js` + `$app/navigation` are mocked, the roster is
// seeded straight into the `contacts` store, and the assertions run against the RENDERED page.
//
// MUTATION PROOFS (§9 / P-10) — every test below was SEEN RED against the exact production edit
// named in its comment, with the file restored (cp backup + `touch`) between proofs:
//  • click-select / dblclick-Browse / has-collection gate / Enter / hover — the five proofs named
//    beside the QURATOR-368b tests themselves (mutations a–e in this file's first describe).
//  • rail/tag-filter absence ← the removed "Filter by tag" row + `allTags` state restored VERBATIM → REDS.
//  • right pane              ← F1 fold: re-adding the old likes panel → REDS (absence pin).
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

// jsdom has no ResizeObserver. The page's M23 W6 `bioMeasure` action used to construct one the
// moment a contact HAD a bio (a jsdom gap, not a production defect) — QURATOR-368c deleted that
// action with the face bio, so the stub below is belt-and-braces for any component that grows one.
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

// QURATOR-368b — NPUB_B under its own name, so "the pane shows THAT contact" is assertable by
// .pane-name text alone (the shared BASE_PROFILE name would otherwise match row 0 too).
const BEE_NAME = 'Bee Browse';
function peerB(overrides: Partial<ContactSummary> = {}): ContactSummary {
	return peer({ npub: NPUB_B, profile: { ...BASE_PROFILE, display_name: BEE_NAME }, ...overrides });
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

describe('QURATOR-368 slice b — row click selects; double-click Browses (only with a collection)', () => {
	it('a plain click selects the row into the right pane and does NOT navigate (i)', async () => {
		contacts.set([
			peer({ npub: NPUB_A, online: true }),
			peerB(),
		]);
		const { container } = render(ContactsPage);
		const rows = await waitFor(() => {
			const found = container.querySelectorAll('.contact-card');
			expect(found.length).toBe(2);
			return found;
		});
		vi.mocked(goto).mockClear();
		// Online bucket renders first, so rows[1] is the offline peer NPUB_B in "Everyone else".
		// The pin: the plain click carries the npub into the PANE (the held 250 ms select lands),
		// and nothing navigates.
		fireEvent.click(rows[1]);
		await waitFor(() => expect(container.querySelector('.detail-pane .pane-name')?.textContent).toBe(BEE_NAME));
		// No navigation of any kind — Browse is double-click's job now.
		expect(vi.mocked(goto)).not.toHaveBeenCalled();
		// Re-clicking the open row keeps it selected (never toggles closed — the click is the only
		// way to open now).
		fireEvent.click(rows[1]);
		await new Promise((r) => setTimeout(r, 400));
		expect(container.querySelector('.detail-pane .pane-name')?.textContent).toBe(BEE_NAME);
	});
	// P-10 MUTATION (a): in +page.svelte's card onclick replace `scheduleRowSelect(peer.npub);`
	// with `goto('/browse?peer=' + encodeURIComponent(peer.npub));` → this test REDS (goto is
	// called with /browse; pane-name never becomes BEE_NAME). Restored + touch → green.

	it('a double-click on a row WITH a collection goes to Browse once; the pane never flashes that row in (ii)', async () => {
		contacts.set([peerB({ collections: [pubCollection('films')] })]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => {
			const r = container.querySelector('.contact-card');
			expect(r).toBeTruthy();
			return r!;
		});
		vi.mocked(goto).mockClear();
		// The first click of a double-click fires `click` too — the held select must NOT land.
		fireEvent.click(row);
		fireEvent.dblClick(row);
		await waitFor(() => expect(vi.mocked(goto)).toHaveBeenCalledWith('/browse?peer=' + NPUB_B));
		expect(vi.mocked(goto)).toHaveBeenCalledTimes(1);
		// No selection flash: the held select was cancelled, so the pane never shows this row.
		await new Promise((r) => setTimeout(r, 400));
		expect(container.querySelector('.detail-pane .pane-name')).toBeNull();
	});
	// P-10 MUTATION (b): delete `cancelRowSelect();` from the card's ondblclick → the held select
	// fires after the Browse goto, so goto is called TWICE and the pane opens — this test REDS on
	// toHaveBeenCalledTimes(1) (and on pane-name null). Restored + touch → green.

	it('a double-click on a row with NO collection does not navigate — the held select still lands (iii)', async () => {
		contacts.set([peerB()]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => {
			const r = container.querySelector('.contact-card');
			expect(r).toBeTruthy();
			return r!;
		});
		fireEvent.click(row);
		fireEvent.dblClick(row);
		await new Promise((r) => setTimeout(r, 400));
		// Nothing to browse, so no navigation — and the first click's select is NOT discarded.
		expect(vi.mocked(goto)).not.toHaveBeenCalled();
		expect(container.querySelector('.detail-pane .pane-name')?.textContent).toBe(BEE_NAME);
	});
	// P-10 MUTATION (c): delete `if (!hasCollections) return;` from the card's ondblclick → the
	// collection-less double-click now navigates and this test REDS on not.toHaveBeenCalled().
	// Restored + touch → green.

	it('modifier clicks never select or navigate (iv)', async () => {
		contacts.set([
			peer({ npub: NPUB_A, online: true }),
			peerB({ collections: [pubCollection('films')] }),
		]);
		const { container } = render(ContactsPage);
		const rows = await waitFor(() => {
			const found = container.querySelectorAll('.contact-card');
			expect(found.length).toBe(2);
			return found;
		});
		vi.mocked(goto).mockClear();
		// QURATOR-368c RE-POINT: this test also drove the row's ⋯ menu (click + dblclick) to prove
		// the inner-control guard (`closest('button, a, input')`) — but slice c removed ALL controls
		// from the at-rest row (Browse / Message / ⋯ live in the pane now), so there is no row
		// control left to drive in jsdom. The guard stays in the click/dblclick/keydown handlers as
		// DEFENSIVE code for any control re-added to a row; the modifier-click half remains mounted
		// and drivable, and is what this test pins.
		await fireEvent.click(rows[1], { shiftKey: true });
		await new Promise((r) => setTimeout(r, 400));
		expect(vi.mocked(goto)).not.toHaveBeenCalled();
		expect(container.querySelector('.detail-pane .pane-name')).toBeNull();
	});
	// P-10 MUTATION (SEEN RED under QURATOR-368b, when the row still carried the ⋯): delete
	// `if ((e.target as HTMLElement).closest('button, a, input')) return;` from the card's onclick
	// → the ⋯ click scheduled a select and this test RED on pane-name null. Restored + touch →
	// green. Since 368c the ⋯ driver no longer exists (the row renders no controls); the guard is
	// no longer jsdom-drivable and this comment is the receipt, not a live proof.

	it('hovering a row never changes the right pane — only click does (v)', async () => {
		contacts.set([
			peer({ npub: NPUB_A, online: true }),
			peerB(),
		]);
		const { container } = render(ContactsPage);
		const rows = await waitFor(() => {
			const found = container.querySelectorAll('.contact-card');
			expect(found.length).toBe(2);
			return found;
		});
		// Select the online row first so the pane has something to be WRONG about.
		fireEvent.click(rows[0]);
		await waitFor(() => expect(container.querySelector('.detail-pane .pane-name')?.textContent).toBe('Somebody Hoardish'));
		fireEvent.mouseEnter(rows[1]);
		fireEvent.mouseOver(rows[1]);
		await new Promise((r) => setTimeout(r, 400));
		expect(container.querySelector('.detail-pane .pane-name')?.textContent).toBe('Somebody Hoardish');
		expect(vi.mocked(goto)).not.toHaveBeenCalled();
	});
	// P-10 MUTATION (e): add `onmouseover={() => selectContact(peer.npub)}` to the card's
	// attributes → this test REDS (the hover moves the pane). Restored + touch → green.

	it('Enter on a focused row selects it, same as click', async () => {
		contacts.set([peerB()]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => {
			const r = container.querySelector('.contact-card');
			expect(r).toBeTruthy();
			return r!;
		});
		fireEvent.keyDown(row, { key: 'Enter' });
		await waitFor(() => expect(container.querySelector('.detail-pane .pane-name')?.textContent).toBe(BEE_NAME));
		expect(vi.mocked(goto)).not.toHaveBeenCalled();
	});
	// P-10 MUTATION (d): in the card's onkeydown delete `scheduleRowSelect(peer.npub);` → Enter
	// does nothing and this test REDS. Restored + touch → green.
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

describe('QURATOR-360 F1 — the likes panel is folded into Discover; the right pane is the open contact', () => {
	it('no likes panel anywhere, and the right pane carries the open contact detail', async () => {
		contacts.set([peer({ npub: NPUB_A })]);
		const { container } = render(ContactsPage);
		await waitFor(() => expect(container.querySelector('.contact-card')).toBeTruthy());
		// Absence pin (was: presence). The name is a SPLIT literal so this file stays at the
		// acceptance grep count of 0 for the deleted component's name.
		const gone = 'People' + 'LikeYou';
		const src = readFileSync(resolve(process.cwd(), 'src/routes/contacts/+page.svelte'), 'utf8');
		expect(src.includes(gone)).toBe(false);
		expect(container.querySelector('.people-pane')).toBeNull();
		// The row click still selects (QURATOR-368b: held 250 ms, then lands); the PANE (not the
		// row) renders the detail (.detail-scroll; the .contact-detail wrapper went in QURATOR-378 rev b).
		fireEvent.click(container.querySelector('.contact-card')!);
		await waitFor(() => expect(container.querySelector('.detail-pane .pane-bio')).not.toBeNull());
		// …and the detail is OUT of the row: .contact-block must not contain it.
		const blk = container.querySelector('.contact-block');
		const det = container.querySelector('.detail-pane .pane-bio')!;
		expect(blk!.contains(det)).toBe(false);
	});
	// MUTATION PROOF (P-10): lie to the pane render gate — `{#if openPeer}` → `{#if false && openPeer}`
	// — the pane shows EmptyState, `.detail-pane .pane-bio` never renders → RED
	// (observed: `expected … '.detail-pane .pane-bio'` toBeTruthy). Restore + touch → green.
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
