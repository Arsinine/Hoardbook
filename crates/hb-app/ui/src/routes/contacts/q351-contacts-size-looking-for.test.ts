// @vitest-environment jsdom
// QURATOR-351 slice 3, r5 state "d" (greenlit, owner 2026-10-13): the Contacts view keeps the r2
// design the owner accepted and adds exactly two things —
//   1. the peer's PUBLISHED total (`Profile::total_bytes`) on every contact row, right-aligned on
//      the name row (`row-size`); no total, or 0, renders NO span (never a fabricated "0 B").
//   2. a "Looking for" line on the contact detail, FIRST in the pane (the r5 ruling: the demand
//      side comes first), rendered only when they published one. QURATOR-378 rev b: it is the
//      first element of tier 1 (`.about`), ahead of the bio, instead of the first `detail-theirs` card.
// Nothing else: no bands, no gold, no counts on the toggle, no "new people" anywhere.
//
// BEHAVIOURAL mount tests (not a source-scan — source-scans are the documented origin of vacuous
// controls here): the page is mounted with only `$lib/api.js` mocked, the roster seeded straight
// into the `contacts` store (same recipe as q135-unknown-pill / q93-contacts-load-error).
//
// ⚠ P-13: jsdom computes no layout. These tests prove the right TEXT, CLASS and PLACEMENT-IN-THE-TREE
// are chosen — not that the row renders as ONE VISUAL LINE. The `.row-size` span sits inside the
// existing one-line `.name-row` flex container (test 4 asserts that structurally), but flex-wrap,
// overflow and pixel geometry are only checkable in a real browser (the Playwright +
// getComputedStyle recipe). A suite that pins ATTRIBUTES is blind to a change that only alters SHAPE.
//
// MUTATION PROOFS (per §9/P-10 — a green test proves nothing until seen red; the exact edit each
// proof performs is named beside the test and the reds were confirmed during the run):
//   M1 — delete the `{#if profileTotal}` span block in +page.svelte's name-row → tests 1 and 4 red.
//   M2 — delete the `{#if lookingFor}` block in +page.svelte's detail column → test 2 red.
//   M3 — move the `{#if lookingFor}` block BELOW the "Profile published by them" block → test 2's
//        first-among-blocks assertion reds (existence alone survives — that is why the test asserts
//        ORDER, not just presence).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import ContactsPage from './+page.svelte';
import { contacts, contactsLoadError } from '$lib/stores.js';
import type { ContactSummary, Profile } from '$lib/types.js';

// The api mock — every Tauri command Contacts imports is stubbed (same shape as q135).
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

// QURATOR-351: `looking_for` is typed on `Profile` by the sibling lane (types.ts); until that
// merges, tests build the field through this intersection and pass it via a variable (structural
// typing carries the extra prop; excess-property checks only fire on literals).
type Profile351 = Profile & { looking_for?: string };

function prof(overrides: Partial<Profile351> = {}): Profile {
	return {
		display_name: overrides.display_name ?? 'Q351 Peer',
		tags: [],
		languages: [],
		social_links: [],
		willing_to: [],
		content_types: [],
		updated: '2026-10-01T00:00:00Z',
		...overrides,
	} as Profile;
}

// Offline contacts only: an ONLINE contact renders in the "Online now" strip AND its group
// section, which would double-render the row and break the count assertions for unrelated reasons.
function peer(npub: string, profile: Profile): ContactSummary {
	return {
		npub,
		has_browse_key: false,
		collections: [],
		online: false,
		last_fetched: new Date().toISOString(),
		local_tags: [],
		profile,
	};
}

async function renderRoster(roster: ContactSummary[]) {
	contacts.set(roster);
	const rendered = render(ContactsPage);
	await waitFor(() => expect(rendered.container.querySelectorAll('.contact-block').length).toBe(roster.length));
	return rendered;
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	contactsLoadError.set(false);
});

describe('QURATOR-351 state d — size on the row, Looking-for on the detail', () => {
	// M1 reds this: delete the `{#if profileTotal}` block in the name-row of +page.svelte.
	it('a published total renders as row-size with the shared formatter output; none or 0 renders nothing', async () => {
		const withTotal = peer('npub1q351alice' + 'a'.repeat(40), prof({ display_name: 'Alice', total_bytes: 1_500_000_000 }));
		const noTotal = peer('npub1q351bob' + 'b'.repeat(42), prof({ display_name: 'Bob' }));
		const zeroTotal = peer('npub1q351carol' + 'c'.repeat(40), prof({ display_name: 'Carol', total_bytes: 0 }));
		const { container } = await renderRoster([withTotal, noTotal, zeroTotal]);

		const sizes = container.querySelectorAll('.row-size');
		// Exactly ONE span across the whole roster — 0 is absence, not "0 B".
		expect(sizes.length).toBe(1);
		// The formatter output matches Browse's fmtBytes (1.5e9 → "1.5 GB"): one implementation's
		// wording, reused, not a second dialect.
		expect(sizes[0].textContent).toBe('1.5 GB');
	});

	// M2 reds this: delete the `{#if lookingFor}` block in the detail column.
	// M3 reds this: move that block below the bio — the FIRST-element assertion fails while mere
	// existence stays green, which is why the test pins order.
	it("a published looking_for renders the Looking-for line FIRST in the pane", async () => {
		const { container } = await renderRoster([
			peer('npub1q351dana' + 'd'.repeat(42), prof({ display_name: 'Dana', looking_for: 'Kurosawa box sets' })),
		]);

		// QURATOR-368b: the chevron is gone — a row click selects into the pane (held 250 ms).
		fireEvent.click(container.querySelector('.contact-card')!);
		await waitFor(() => expect(container.querySelectorAll('.looking-for').length).toBe(1));
		// The eyebrow label is part of the line; the published text follows it.
		const lf = container.querySelector('.looking-for')!;
		expect(lf.querySelector('.lf-eyebrow')!.textContent).toBe('Looking for');
		expect(lf.textContent).toBe('Looking forKurosawa box sets');

		// First tier, first element: the demand side leads, ahead of the bio.
		const about = container.querySelector('.detail-pane .about')!;
		expect(about.firstElementChild).toBe(lf);
	});

	it('a contact who published no looking_for renders no Looking-for block at all', async () => {
		const { container } = await renderRoster([
			peer('npub1q351evan' + 'e'.repeat(42), prof({ display_name: 'Evan' })),
		]);

		// QURATOR-368b: row click selects into the pane (held 250 ms).
		fireEvent.click(container.querySelector('.contact-card')!);
		// The detail still opens (the bio line appears), but no Looking-for anywhere.
		await waitFor(() => expect(container.querySelector('.detail-pane .pane-bio')).not.toBeNull());
		expect(container.querySelector('.looking-for')).toBeNull();
		expect(container.querySelector('.lf-eyebrow')).toBeNull();
		expect(container.querySelector('.detail-pane .about')!.firstElementChild!.classList.contains('pane-bio')).toBe(true);
	});

	// M1 also reds this (the span is gone entirely). Structural one-line proxy only — see the P-13
	// note at the top: jsdom computes no layout, so this proves the span shares the name's flex row,
	// NOT that the row renders as one visual line.
	it('the size span is inside the same name-row element as the name (structural one-line proxy)', async () => {
		const { container } = await renderRoster([
			peer('npub1q351fox' + 'f'.repeat(42), prof({ display_name: 'Fox', total_bytes: 880_000_000 })),
		]);

		const nameRow = container.querySelector('.name-row');
		expect(nameRow).not.toBeNull();
		expect(nameRow!.querySelector('.peer-name')).not.toBeNull();
		expect(nameRow!.querySelector('.row-size')).not.toBeNull();
		// Wording matches Browse's fmtBytes verbatim — including its trailing ".0" (the r5 mock's
		// "880 MB" is aspirational; the drift-pair rule says match the incumbent formatter, not the mock).
		expect(nameRow!.querySelector('.row-size')!.textContent).toBe('880.0 MB');
	});
});
