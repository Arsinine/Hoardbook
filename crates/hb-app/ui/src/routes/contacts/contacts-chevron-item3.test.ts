// @vitest-environment jsdom
// Devtest 2026-08-26 item 3 — owner: "The Expand chevron in the contacts card requires a redesign,
// right now its showing duplicated information. It should contain the rest of the bio stuff that the
// base card didnt have room for like region, contact, other socials if any, willing to etc."
// The artifact's three rulings were greenlit wholesale ("Behind the Chevron, greenlight the proposed
// changes"): 01 the panel's group editor is deleted, 02 peer strings render inert + copyable, 03 an
// empty profile says so rather than collapsing.
//
// This is a MOUNT test. The defect it pins is not "a field is wrong" but "seven fields crossed the
// Tauri boundary on every contact and were rendered NOWHERE" — a source-scan for `p.location` would
// have been satisfied by the type definition alone. Only a render proves a value reaches the screen.
//
// MUTATION PROBES, run 2026-08-27 against PRODUCTION (contacts/+page.svelte), one change at a time,
// each reverted and the revert diff-verified byte-identical against a backup. Each probe ran this
// file AND contacts-w5b.test.ts (19 tests total) so a probe that reddened something unrelated would
// show. Every one reddened EXACTLY its one named test — 1 failed | 18 passed, four times:
//   a) delete the {#if p.location} row from the facts <dl>
//        -> "renders every profile fact the face has no room for"      RED
//   b) insert a .group-edit-list checkbox editor back into the panel
//        -> "the panel holds no second group editor (ruling 01)"       RED
//        (w5b's "the chip row lives on the face ONLY" stayed green, correctly: it pins the
//         read-only CHIP row, a different affordance from the checkbox editor this probe added.)
//   c) render email as <a href="mailto:{p.email}"> instead of inert mono + copy
//        -> "peer-published strings are inert, never links (ruling 02)" RED
//   d) drop the {:else} branch so an empty profile renders nothing
//        -> "an empty profile is stated, not hidden (ruling 03)"        RED
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ContactsPage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { CachedPeer, Profile } from '$lib/types.js';

// jsdom has no ResizeObserver, and `bioMeasure` (the M23 W6 bio-overflow action) constructs one the
// moment a contact HAS a bio. No pre-existing mount test in this directory renders a peer with a
// bio, so the whole clamp/measure path had never been exercised at mount level — this stub is what
// lets it run at all. It is a jsdom gap, not a production defect: the class exists in every browser
// Tauri ships. jsdom also computes no layout, so `measure()` reads 0/0 and the "more ⌄" control is
// never shown here; nothing below asserts on it.
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

import { groupsGet } from '$lib/api.js';
const groupsGetMock = groupsGet as unknown as ReturnType<typeof vi.fn>;

const NPUB = 'npub1full' + 'f'.repeat(53);
const BIO = 'Collects VHS-era Australian television and regional broadcast tape.';

/** A peer carrying ALL SEVEN of the fields that used to render nowhere. */
const FULL: CachedPeer = {
	npub: NPUB,
	collections: [],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: {
		display_name: 'Fulla Details',
		bio: BIO,
		tags: ['vhs'],
		since: 2011, // a YEAR, not a date string — Profile.since is a number (home's <input type="number" min=1990>)
		est_size: '4 TB',
		languages: ['English', 'Japanese'],
		contact_hint: 'ping me on the forum, same handle',
		email: 'fulla@example.invalid',
		location: 'Melbourne, AU',
		social_links: [{ platform: 'mastodon', handle: '@fulla@aus.social' }],
		willing_to: ['trade', 'seed on request'],
		content_types: ['video'],
		updated: '2026-08-01T00:00:00Z',
	} as Profile,
};

/** Same shape, but they published nothing beyond a name — ruling 03's case. */
const BARE: CachedPeer = {
	npub: 'npub1bare' + 'b'.repeat(53),
	collections: [],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: {
		display_name: 'Bare Minimum',
		tags: [],
		languages: [],
		social_links: [],
		willing_to: [],
		content_types: [],
		updated: '2026-08-01T00:00:00Z',
	} as Profile,
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

/** Mount, wait for the page's own load to settle, then open the one card's chevron. */
async function openPanel(peer: CachedPeer): Promise<HTMLElement> {
	contacts.set([peer]);
	const { getByRole } = render(ContactsPage);
	await waitFor(() => expect(groupsGetMock).toHaveBeenCalled());
	await tick();
	await fireEvent.click(getByRole('button', { name: 'Toggle details' }));
	await tick();
	const panel = document.querySelector<HTMLElement>('.contact-detail');
	expect(panel, 'the chevron did not open a .contact-detail panel').not.toBeNull();
	return panel!;
}

describe('devtest item 3 — the chevron panel is the REST of the profile', () => {
	it('renders every profile fact the face has no room for', async () => {
		const panel = await openPanel(FULL);
		const text = panel.textContent ?? '';
		// Each of the seven, by the value the backend sent — a label alone would pass on an empty <dd>.
		expect(text, 'location missing').toContain('Melbourne, AU');
		expect(text, 'languages missing').toContain('English');
		expect(text, 'languages missing').toContain('Japanese');
		expect(text, 'willing_to missing').toContain('trade');
		expect(text, 'willing_to missing').toContain('seed on request');
		expect(text, 'contact_hint missing').toContain('ping me on the forum, same handle');
		expect(text, 'email missing').toContain('fulla@example.invalid');
		expect(text, 'social handle missing').toContain('@fulla@aus.social');
		expect(text, 'since missing').toContain('2011');
		// The npub: M21 W4 took it off the face and the old panel comment claimed it lived here. It
		// did not. This is that claim made true.
		expect(text, 'npub missing').toContain(NPUB);
	});

	it('repeats nothing the card face already shows — that was the actual complaint', async () => {
		const panel = await openPanel(FULL);
		const card = panel.closest('.contact-card') ?? document.body;
		// The bio renders on the face (row 3, clamped). Exactly once in the whole card.
		const occurrences = (card.textContent ?? '').split(BIO).length - 1;
		expect(occurrences, 'the bio appears more than once in the card').toBe(1);
		// est_size is on the face too, so it is deliberately NOT in the facts list.
		expect(panel.textContent ?? '', 'est_size duplicated into the panel').not.toContain('4 TB');
	});

	it('the panel holds no second group editor (ruling 01)', async () => {
		// Two groups exist and the peer is in one, so a surviving editor would have something to draw.
		groupsGetMock.mockResolvedValue([
			{ name: 'Film', pubkeys: [NPUB] },
			{ name: 'Music', pubkeys: [] },
		]);
		const panel = await openPanel(FULL);
		expect(panel.querySelector('.group-edit-list'), 'the checkbox editor survived').toBeNull();
		expect(panel.querySelector('.group-pill'), 'the read-only chip row survived').toBeNull();
		// The real invariant behind both: no checkbox in this panel is a group toggle. The audience
		// checkbox IS expected here, so count group-named ones rather than checkboxes outright.
		const labels = [...panel.querySelectorAll('label')].map((l) => l.textContent ?? '');
		expect(labels.some((t) => t.includes('Film') || t.includes('Music'))).toBe(false);
		// …while the face's `+` — the ONE surviving editor's entry point — is still there.
		expect(
			document.querySelector('.group-add-btn'),
			'deleting the panel editor also took the popover trigger',
		).not.toBeNull();
	});

	it('peer-published strings are inert, never links (ruling 02)', async () => {
		const panel = await openPanel(FULL);
		// A peer controls these strings. Rendering them as href would let a contact card open a mail
		// client or a browser on a value somebody else chose.
		for (const a of panel.querySelectorAll('a')) {
			const href = a.getAttribute('href') ?? '';
			expect(href.startsWith('mailto:'), `panel link to ${href}`).toBe(false);
			expect(/^https?:/.test(href), `panel link to ${href}`).toBe(false);
		}
		// They are copyable instead — the same affordance the npub already had.
		expect(panel.querySelectorAll('.copy-btn').length).toBeGreaterThanOrEqual(3);
	});

	it('an empty profile is stated, not hidden (ruling 03)', async () => {
		const panel = await openPanel(BARE);
		// QURATOR-93's confident-empty shape is what this avoids: a block that renders nothing is
		// indistinguishable from a panel that failed to load.
		expect(panel.textContent ?? '').toContain('No profile details published.');
	});

	it('your own local notes stay reachable and stay marked as local', async () => {
		const panel = await openPanel(FULL);
		const text = panel.textContent ?? '';
		expect(text).toContain('local to this machine');
		// M21 W5: the Private audience toggle is per-contact and explicitly NOT group-derived. It
		// lives with the local notes, and moving the group editor out must not have taken it along.
		expect(text).toContain('Receives my Private collections');
	});
});
