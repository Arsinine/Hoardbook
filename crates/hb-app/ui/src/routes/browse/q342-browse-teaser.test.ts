// @vitest-environment jsdom
// QURATOR-342 D2 — Browse's unreadable-peer TEASER + the title-search strip mount.
//
// Owner rulings bound (2026-09-27/28): no read-state screens or pills — "In browser if you can
// read, you can read. If you cant you get a teaser with the reason why"; the reason copy is
// EXACTLY "Readable once your hoard reaches X" (X = THEIR total); "the collection name is the
// teaser" (names + sizes from `profile.teaser_collections`); NO Ask-for-access affordance on an
// unreadable peer.
//
// BEHAVIOURAL mount tests (the q92/q102/q134 pattern): the real page is mounted with only
// `$lib/api.js` mocked, selection happens through the `/browse?peer=` deep-link (which routes
// through selectPeer exactly as production does), and `refreshContact` resolves the fixture.
// TitleSearch itself is mocked with ./TitleSearchProbe.svelte (see that file for why): the real
// component is lane D1's, currently a stub that renders nothing, so the strip's mount + the
// onbrowse WIRING are proven through the probe.
//
// Every test below was SEEN RED (mutation beside each `it`; full recipe in the task report).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { ContactSummary } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn(),
	groupsCreateWithMembers: vi.fn(),
	groupsAssign: vi.fn(),
	groupsDelete: vi.fn(),
	groupsUnassign: vi.fn(),
	contactUpdateGroups: vi.fn(),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	applyKeyGrants: vi.fn().mockResolvedValue([]),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

// TitleSearch stand-in: async factory + dynamic import (no hoisting hazards).
vi.mock('$lib/components/TitleSearch.svelte', async () => {
	const { default: Probe } = await import('./TitleSearchProbe.svelte');
	return { default: Probe };
});

// $page stub: the `/browse?peer=<npub>` deep-link selects the peer (the q134 pattern). The npub
// is inlined because vi.mock is hoisted above every const.
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse?peer=npub1q342q342q342q342q342q342q342q342q342q342q342q342q342') }) };
});
vi.mock('$app/stores', () => stubPage);

// bech32-safe fixture id (charset excludes 1/b/i/o after the separator); length matches the npub
// in the stubbed $page URL — peerFromQuery matches on the full string.
const PEER_NPUB = 'npub1q342q342q342q342q342q342q342q342q342q342q342q342q342';
const PEER_B_NPUB = 'npub1q342btctq342btctq342btctq342btctq342btctq342btctq342';

const TB = 1024 ** 4;

const PROFILE_BASE = {
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
};

/** KEYLESS sealed peer — the classic unreadable state — carrying the full teaser payload. */
function sealedPeerWithTeaser(): ContactSummary {
	return {
		npub: PEER_NPUB,
		has_browse_key: false,
		collections: [],
		online: false,
		last_fetched: '2026-08-01T00:00:00Z',
		local_tags: [],
		listings_state: 'Sealed',
		read_state: { kind: 'locked', need_bytes: 45 * TB },
		profile: {
			...PROFILE_BASE,
			display_name: 'Locked Peer',
			est_size: '~44.5 TB',
			teaser_collections: [
				{ name: 'Lossless Music Archive', bytes: 12 * TB },
				{ name: 'World Cinema 4K', bytes: 8 * TB },
			],
		},
	} as unknown as ContactSummary;
}

/** KEYED peer locked by the v5 size rule ALONE (read_state locked, listings state irrelevant). */
function keyedRuleLockedPeer(): ContactSummary {
	return {
		npub: PEER_NPUB,
		has_browse_key: true,
		collections: [],
		online: false,
		last_fetched: '2026-08-01T00:00:00Z',
		local_tags: [],
		read_state: { kind: 'locked', need_bytes: 45 * TB },
		profile: {
			...PROFILE_BASE,
			display_name: 'Outgrown Peer',
			teaser_collections: [{ name: 'Retro Game Preservation', bytes: 7 * TB }],
		},
	} as unknown as ContactSummary;
}

/** Plain Fetched peer — tri-state control (must NOT render the teaser). */
function fetchedPeer(npub: string): ContactSummary {
	return {
		npub,
		has_browse_key: false,
		collections: [],
		online: false,
		last_fetched: '2026-08-01T00:00:00Z',
		local_tags: [],
		listings_state: 'Fetched',
		profile: { ...PROFILE_BASE, display_name: 'Fetched Peer' },
	} as unknown as ContactSummary;
}

/** Keyed peer WITH collections — the readable grid, for the onbrowse wiring probe. */
function keyedPeerWithCollections(npub: string, name: string): ContactSummary {
	return {
		npub,
		has_browse_key: true,
		collections: [{ slug: 'b-cols', path_alias: name, item_count: 3, total_bytes: 1024, content_types: [], tags: [], languages: [] }],
		online: false,
		last_fetched: '2026-08-01T00:00:00Z',
		local_tags: [],
		profile: { ...PROFILE_BASE, display_name: name },
	} as unknown as ContactSummary;
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	delete (window as unknown as { __TS_PROBE_NPUB__?: string }).__TS_PROBE_NPUB__;
});

import { refreshContact } from '$lib/api.js';
const refreshMock = refreshContact as unknown as ReturnType<typeof vi.fn>;

describe('QURATOR-342 D2 — unreadable-peer teaser + title-search strip', () => {
	it('sealed peer: teaser names + sizes, the EXACT reason sentence with THEIR total, "their hoard" header — never the lock screen', async () => {
		// MUTATION (seen red): reverting the teaser branch to the old 🔒 "Listings locked" +
		// Ask-for-access block removed the reason line, the header and both names.
		refreshMock.mockResolvedValue(sealedPeerWithTeaser());
		contacts.set([sealedPeerWithTeaser()]);
		const { getByText } = render(BrowsePage);
		await tick();

		// The exact ruling copy — X is `read_state.need_bytes` (THEIR total), not ours.
		await waitFor(() => expect(getByText('Readable once your hoard reaches 45.0 TB')).toBeTruthy());
		// Header line with THEIR total (sum of the teaser collections: 12 + 8 = 20 TB).
		expect(getByText('their hoard 20.0 TB')).toBeTruthy();
		// Names + per-collection sizes, formatted with fmtLargestUnit.
		expect(getByText('Lossless Music Archive')).toBeTruthy();
		expect(getByText('World Cinema 4K')).toBeTruthy();
		expect(document.body.textContent).toContain('12.0 TB');
		expect(document.body.textContent).toContain('8.0 TB');
		// The retired lock screen must not render in any form. (The People list's per-peer
		// access-lock badges are a DIFFERENT surface and legitimately still show 🔒 — the
		// assertion must be the retired empty-state copy, never a whole-page sweep that the
		// badge would satisfy. Assert the teaser block itself carries none of it.)
		expect(document.body.textContent).not.toContain('Listings locked');
		expect(document.querySelector('.teaser-state')?.textContent ?? '').not.toContain('🔒');
		expect(document.querySelector('.teaser-state')?.textContent ?? '').not.toContain('Ask for access');
		expect(document.body.textContent).not.toContain('Ask for access');
	});

	it('keyed peer locked by the v5 size rule ALONE: teaser, and NO "Ask for access" anywhere on the page', async () => {
		// MUTATION (seen red): dropping the `read_state?.kind === 'locked'` half of the
		// `peerUnreadable` derivation left this keyed peer falling through to "No public
		// collections" — the teaser (and this test) went red while the sealed-peer test above
		// stayed green (disjoint needles, one mutation each).
		refreshMock.mockResolvedValue(keyedRuleLockedPeer());
		contacts.set([keyedRuleLockedPeer()]);
		const { getByText } = render(BrowsePage);
		await tick();

		// The teaser is driven by read_state alone; the profile has no est_size, so the header
		// comes from the teaser sum (7 TB) and the reason from need_bytes (45 TB).
		await waitFor(() => expect(getByText('Readable once your hoard reaches 45.0 TB')).toBeTruthy());
		expect(getByText('their hoard 7.0 TB')).toBeTruthy();
		expect(getByText('Retro Game Preservation')).toBeTruthy();
		// NO ask affordance anywhere — every branch, not just this one.
		expect(document.body.textContent).not.toContain('Ask for access');
		expect(document.body.textContent).not.toContain('Listings locked');
	});

	it('teaser_collections empty/absent: ONLY the reason line renders (generic line, never a fake number)', async () => {
		// MUTATION (seen red): making `lockReasonLine` fall back to `fmtLargestUnit(0)` for the
		// nothing-known case fabricated "0.0 B" — the generic sentence disappeared and this red.
		const p = sealedPeerWithTeaser();
		p.profile = { ...PROFILE_BASE, display_name: 'Bare Locked' } as ContactSummary['profile'];
		// "neither is known" = no read_state AND no profile size signal at all.
		p.read_state = undefined;
		refreshMock.mockResolvedValue(p);
		contacts.set([p]);
		const { getByText } = render(BrowsePage);
		await tick();

		await waitFor(() => expect(getByText("Their collections aren't readable yet.")).toBeTruthy());
		// Nothing is known — no header number, no grid, no fabricated figure.
		expect(document.body.textContent).not.toContain('their hoard');
		expect(document.body.textContent).not.toContain('Readable once your hoard reaches');
	});

	it('TitleSearch is mounted in the right pane and its onbrowse selects the peer through selectPeer', async () => {
		// MUTATION (seen red): removing the `.title-search-strip` mount from the right-panel
		// template reds BOTH asserts — the probe button never renders, and no selection fires.
		const a = fetchedPeer(PEER_NPUB);
		const b = keyedPeerWithCollections(PEER_B_NPUB, 'B Cols');
		refreshMock.mockImplementation(async (npub: string) => {
			return npub === PEER_B_NPUB ? b : a;
		});
		contacts.set([a, b]);
		const { getByText, getByTestId } = render(BrowsePage);
		await tick();

		// A (deep-linked) settled; the probe proves the strip is mounted.
		await waitFor(() => expect(getByText('No public collections')).toBeTruthy());
		const probe = getByTestId('ts-probe');
		expect(probe).toBeTruthy();

		// Drive the ramp: onbrowse(B) → $contacts lookup → selectPeer → B's grid renders.
		(window as unknown as { __TS_PROBE_NPUB__?: string }).__TS_PROBE_NPUB__ = PEER_B_NPUB;
		await fireEvent.click(probe);
		await waitFor(() => expect(refreshMock).toHaveBeenCalledWith(PEER_B_NPUB));
		// B's collection GRID renders in the right pane (the People list ALSO carries the name —
		// scope the assert to the collection card, not the whole body).
		await waitFor(() => expect(getByText('B Cols', { selector: '.col-card-name' })).toBeTruthy());
	});

	it('FetchFailed still shows the error + Retry — the teaser must not swallow the failure state', async () => {
		// MUTATION (seen red): collapsing the `listingsLoadFailed` derivation to `false` dropped
		// the failed peer into "No public collections" — Retry disappeared and this red, while
		// the teaser tests (Sealed peers) stayed green.
		const p = { ...sealedPeerWithTeaser(), listings_state: 'FetchFailed', read_state: undefined } as unknown as ContactSummary;
		refreshMock.mockResolvedValue(p);
		contacts.set([p]);
		const { getByRole } = render(BrowsePage);
		await tick();

		const retry = await waitFor(() => getByRole('button', { name: 'Retry' }));
		expect(document.body.textContent).not.toContain('Readable once your hoard reaches');
		expect(document.body.textContent).not.toContain("Their collections aren't readable yet.");

		// The retry affordance is wired — the QURATOR-93 machinery, untouched.
		refreshMock.mockClear();
		await fireEvent.click(retry);
		await waitFor(() => expect(refreshMock).toHaveBeenCalledWith(PEER_NPUB));
	});
});
