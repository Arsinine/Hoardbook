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
import type { ContactSummary, Profile } from '$lib/types.js';

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
const FULL: ContactSummary = {
	npub: NPUB,
	has_browse_key: false,
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
const BARE: ContactSummary = {
	npub: 'npub1bare' + 'b'.repeat(53),
	has_browse_key: false,
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

/** Mount, wait for the page's own load to settle, then SELECT the one card (QURATOR-368b: the
 *  chevron is gone — a row CLICK selects into the pane; the click is held 250 ms against a
 *  possible double-click, then the pane renders). */
async function openPanel(peer: ContactSummary): Promise<HTMLElement> {
	contacts.set([peer]);
	render(ContactsPage);
	await waitFor(() => expect(groupsGetMock).toHaveBeenCalled());
	await tick();
	const card = document.querySelector<HTMLElement>('.contact-card');
	expect(card, 'the contact row rendered').not.toBeNull();
	fireEvent.click(card!);
	const panel = await waitFor(() => {
		// QURATOR-378 rev b: the `.contact-detail` wrapper card is gone; the pane's scroller is the panel.
		const found = document.querySelector<HTMLElement>('.detail-pane .detail-scroll');
		expect(found, 'the row click did not select the contact into the pane').not.toBeNull();
		return found!;
	});
	await tick();
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
		// QURATOR-368c: the bio renders exactly ONCE — it moved into the pane (unclamped); the face
		// carries none. (The old copy of this comment said "on the face, row 3, clamped".)
		const occurrences = (card.textContent ?? '').split(BIO).length - 1;
		expect(occurrences, 'the bio appears more than once in the card').toBe(1);
		// est_size renders in the pane HEADER meta (outside .detail-scroll), so the facts list
		// still does not repeat it. (The old copy said "est_size is on the face too".)
		expect(panel.textContent ?? '', 'est_size duplicated into the panel').not.toContain('4 TB');
	});

	it('the panel carries NO group chips and no checkbox editor (ruling 01 + QURATOR-378 rev b)', async () => {
		// Two groups exist and the peer is in one, so a surviving chip row or editor would have
		// something to draw.
		groupsGetMock.mockResolvedValue([
			{ name: 'Film', pubkeys: [NPUB] },
			{ name: 'Music', pubkeys: [] },
		]);
		const panel = await openPanel(FULL);
		expect(panel.querySelector('.group-edit-list'), 'the checkbox editor survived').toBeNull();
		// Inverted a third time. QURATOR-368c moved the chip row INTO the pane; QURATOR-378 rev b
		// (owner: "drop groups") took it out again. The ⋯ menu's "Edit groups…" is the editor's one
		// entry point now, so neither the chips nor their `+` render here.
		// P-10 MUTATION: re-add a `<span class="group-pill">` row to the pane → REDS.
		expect(panel.querySelectorAll('.group-pill').length, 'the pane grew a group chip row again').toBe(0);
		expect(panel.querySelector('.group-add-btn'), 'the pane `+` came back').toBeNull();
		expect(panel.textContent ?? '', 'a group name rendered in the pane').not.toContain('Film');
		// No checkbox in this panel is a group toggle (the audience checkbox IS expected).
		const labels = [...panel.querySelectorAll('label')].map((l) => l.textContent ?? '');
		expect(labels.some((t) => t.includes('Film') || t.includes('Music'))).toBe(false);
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

	it('your own tags stay reachable and stay marked as yours', async () => {
		const panel = await openPanel({ ...FULL, local_tags: ['met at con'] });
		const text = panel.textContent ?? '';
		// QURATOR-378 rev b: the "local to this machine" label is gone — the owner's ruling is that your
		// tags GLOW ("User added tags glow green"), so the ownership mark is the `.local-tag` class the
		// glow is drawn on, distinct from their published `.peer-tag` chips.
		const mine = panel.querySelector('.local-tag');
		expect(mine?.textContent, 'your tag is not drawn as yours').toContain('met at con');
		expect(mine?.classList.contains('peer-tag'), 'your tag drawn as theirs').toBe(false);
		expect(panel.querySelector('.tag-add-btn'), '+ tag is gone').not.toBeNull();
		// M21 W5: the Private audience toggle is per-contact and explicitly NOT group-derived. It
		// lives with the local notes, and moving the group editor out must not have taken it along.
		expect(text).toContain('Receives my Private collections');
	});
});
