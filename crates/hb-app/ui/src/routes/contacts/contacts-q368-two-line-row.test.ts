// @vitest-environment jsdom
// QURATOR-368 slice c — the contact row AT REST is EXACTLY TWO lines:
//   line 1: name (ellipsises) · collision badge · presence pill · size (right-aligned)
//   line 2: fingerprint words · "N collections" (labelled text — the bare count is gone) ·
//           presence text ("Offline · Last seen …" / "Checking…")
// and the row carries NO bio, NO group chips, NO Browse/Message/⋯ controls — those live in the
// right pane once the contact is selected (slice b's click-select).
//
// P-13: jsdom computes no layout. "Never wraps", "name ellipsises", "size stays right-aligned"
// and "avatar pinned to the top" are CSS claims (`.name-row`, `.peer-name`, `.row-size`,
// `.contact-card { align-items: flex-start }`) that are NOT proven here — only the DOM shape
// (which element sits in which line container) and the chosen text are. The visual claims need
// the Playwright + getComputedStyle recipe.
//
// Mounted, not a source-scan (P-4): only `$lib/api.js` is mocked, the roster is seeded straight
// into the `contacts` store, assertions target the RENDERED DOM.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import ContactsPage from './+page.svelte';
import { contacts, contactsLoadError } from '$lib/stores.js';
import type { ContactSummary, Collection, Profile } from '$lib/types.js';

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
	// fetched_at STAMPED: the poll HAS answered, so an offline beacon-less-beacon peer reads as a
	// real Offline (QURATOR-216), and line 2 may carry the "Offline · Last seen …" text.
	onlineCount: vi.fn().mockResolvedValue({ online: 0, fetched_at: new Date().toISOString(), relay_set: [], fresh: [] }),
	relayStatus: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	privateAudienceList: vi.fn().mockResolvedValue([]),
	privateAudienceSet: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

const NPUB = 'npub1q368' + 'c'.repeat(50);
const BIO = 'Trades 1980s mixtapes and writes long catalogue notes.';
const SEEN_3H_AGO = new Date(Date.now() - 3 * 3_600_000).toISOString();

const PROF: Profile = {
	display_name: 'Dana Two-Lines',
	bio: BIO,
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: [],
	total_bytes: 5_000_000_000,
	updated: '2026-08-01T00:00:00Z',
} as unknown as Profile;

function peer(overrides: Partial<ContactSummary> = {}): ContactSummary {
	return {
		npub: NPUB,
		has_browse_key: false,
		collections: [
			{ slug: 'films', path_alias: 'films', visibility: 'Public', est_size: '1.2 GB', description: 'Movies' },
			{ slug: 'tapes', path_alias: 'tapes', visibility: 'Public', est_size: '800.0 MB', description: 'Mixtapes' },
		] as unknown as Collection[],
		online: false,
		last_fetched: new Date().toISOString(),
		last_presence: SEEN_3H_AGO,
		local_tags: [],
		profile: PROF,
		fingerprint: { words: ['amber', 'otter', 'quill'], colorHex: '#8fa2ff' },
		...overrides,
	};
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	contactsLoadError.set(false);
});

describe('QURATOR-368c — the at-rest row is exactly two lines', () => {
	it('the row DOM has exactly two line containers, with the right contents in each', async () => {
		contacts.set([peer()]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => {
			const found = container.querySelector('.contact-card');
			expect(found, 'the contact row rendered').not.toBeNull();
			return found!;
		});
		const info = row.querySelector('.contact-info')!;
		expect(info, 'the row has a .contact-info').not.toBeNull();

		// EXACTLY two line containers at rest (the drag-outcome line only exists mid-drag).
		const lines = [...info.children].filter((el) => el.tagName === 'DIV');
		expect(lines.length, 'the at-rest row is exactly two lines').toBe(2);

		// LINE 1: name · pill · size right-aligned.
		const line1 = lines[0];
		expect(line1.classList.contains('name-row')).toBe(true);
		expect(line1.querySelector('.peer-name')?.textContent).toBe('Dana Two-Lines');
		// QURATOR-368 owner ruling: line 1 carries the ONLINE pill only — an offline row says
		// "Offline" exactly once, on line 2. P-10 MUTATION (orchestrator): restore the
		// `<span class="pill pill-offline">Offline</span>` branch on line 1 → REDS here.
		expect(line1.querySelector('.pill-offline'), 'no Offline pill on line 1').toBeNull();
		expect(line1.querySelector('.pill-unknown'), 'no Checking pill on line 1').toBeNull();
		expect((row.textContent ?? '').match(/Offline/g)?.length, 'Offline is said once').toBe(1);
		expect(line1.querySelector('.row-size'), 'the size span is line 1').not.toBeNull();

		// LINE 2: fingerprint words · "N collections" (labelled, never the bare count) · presence.
		const line2 = lines[1];
		expect(line2.classList.contains('meta-row')).toBe(true);
		const l2text = line2.textContent ?? '';
		for (const w of ['amber', 'otter', 'quill']) expect(l2text, `fingerprint word ${w}`).toContain(w);
		expect(l2text, 'the count is LABELLED text, not the bare number').toMatch(/2 collections/);
		expect(l2text, 'presence text names the verdict').toMatch(/Offline · Last seen/);
		// …and none of that leaked onto line 1.
		expect(line1.textContent ?? '').not.toMatch(/collections/);
	});

	it('the row contains NO bio, NO group chips, NO Browse/Message/⋯ controls', async () => {
		contacts.set([peer()]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => container.querySelector('.contact-card')!);
		expect(row.querySelector('.bio-row'), 'the face bio is gone (it is in the pane)').toBeNull();
		expect(row.querySelector('.group-pill'), 'the face chips are gone').toBeNull();
		expect(row.querySelector('.row-menu-btn'), 'the ⋯ trigger is gone from the row (pane has it)').toBeNull();
		// No interactive controls at all in the row: nothing may take width while invisible.
		expect(row.querySelectorAll('button, a').length, 'no buttons or links in the at-rest row').toBe(0);
		const rowText = row.textContent ?? '';
		expect(rowText).not.toContain('Message');
		expect(rowText).not.toContain('Browse');
		// The hover popover is gone outright — no element of it anywhere on the page.
		expect(container.querySelectorAll('.collections-popover').length).toBe(0);
	});
});

describe('QURATOR-368c — the moved facts render in the right pane', () => {
	it('selecting the contact shows the full bio and the collections list in .detail-pane', async () => {
		contacts.set([peer()]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => container.querySelector('.contact-card')!);
		fireEvent.click(row);
		// .detail-pane always exists (EmptyState before a selection) — wait for the select to land
		// (the 250 ms double-click hold) by waiting for the pane's bio (QURATOR-378 rev b removed the
		// `.contact-detail` wrapper card).
		const detail = await waitFor(() => {
			const found = container.querySelector('.detail-pane .pane-bio');
			expect(found, 'the row click selected the contact into the pane').not.toBeNull();
			return found!;
		});
		const pane = container.querySelector('.detail-pane')!;

		// The bio, FULL (the clamp and its `more ⌄` control were deleted with the face bio).
		expect(detail.textContent ?? '').toContain(BIO);
		expect(pane.querySelector('.pane-bio')).not.toBeNull();

		// The public collections list is a PLAIN pane list (alias + size per row), not a popover.
		const rows = pane.querySelectorAll('.cpop-row');
		expect(rows.length, 'one pane row per public collection').toBe(2);
		expect(pane.querySelector('.cpop-alias')?.textContent).toBe('films');
	});
});

// The chips' fixture needs a group for the peer; groupsGet is mocked at the top with [] — this
// second render seeds one so a surviving chip row would have something to draw.
import { groupsGet } from '$lib/api.js';
const groupsGetMock = groupsGet as unknown as ReturnType<typeof vi.fn>;

// QURATOR-378 rev b (owner, 2026-10-14: "drop groups"): the chips QURATOR-368c moved into the pane
// left it again. The ⋯ menu's "Edit groups…" and drag-to-group on the list are the group affordances.
describe('QURATOR-378 rev b — no group chips in the pane or the row', () => {
	it('a grouped, coloured contact renders no chip, dot or `+` anywhere', async () => {
		groupsGetMock.mockResolvedValue([{ name: 'Film', pubkeys: [NPUB], color: '#ff0000' }]);
		contacts.set([peer()]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => container.querySelector('.contact-card')!);
		fireEvent.click(row);
		await waitFor(() => expect(container.querySelector('.detail-pane .pane-bio')).not.toBeNull());
		const pane = container.querySelector('.detail-pane')!;
		expect(pane.querySelectorAll('.group-pill, .group-dot, .group-add-btn').length).toBe(0);
		expect(container.querySelector('.contact-card')!.querySelectorAll('.group-pill, .group-add-btn').length).toBe(0);
	});
});

// P-10 MUTATIONS (each SEEN RED against production code, then restored + touched + re-run green):
//   A. two-lines/line-contents family — change the labelled count to the bare number
//      (`{pubCount} collection{...}` → `{pubCount}` in .meta-row) → test 1 REDS on /2 collections/.
//   B. nothing-left-in-the-row family — re-insert a `<div class="bio-row"><p>{BIO}</p></div>` into
//      .contact-info in the ROW → test 2 REDS on `.bio-row` null; adding a Message button reds the
//      `button, a` count.
//   C. pane family — delete the `.pane-cols` public-collections block from .detail-pane → the pane
//      test REDS on .cpop-row. (The Groups block left the pane in QURATOR-378 rev b.)
// P-10 MUTATION: drop the " collections" label (mutation A above) → REDS /2 collections/.
