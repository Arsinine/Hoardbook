// @vitest-environment jsdom
// QURATOR-336 (slice C) — Browse shows an OVERSIZED collection honestly. An oversized collection is
// one whose full list is over the 16 MB transfer limit: the owner published only a breadth-first
// preview, the full list can NEVER be fetched, and `total_items` is a LOWER BOUND. So the paywall
// teaser must NOT print "N more items hidden" (N is unknown) — it prints the bound, "100,000+ items",
// under the title "Preview of a very large collection" — and the footer row carries a red-dot status
// saying the list is too large to send in full.
//
// BEHAVIOURAL mount test (the q79/q159 pattern): the real Browse page mounts with only `$lib/api.js`
// (and the Tauri modules) mocked, the peer is selected through the `/browse?peer=` deep-link, and the
// collection card is clicked open. A source-scan is NOT acceptable for this (CLAUDE.md → P-4): the
// property is what the mounted DOM renders.
//
// Per CLAUDE.md §9 / P-10 each test names, beside it, the production edit that must red it; every
// one of those edits was applied alone, confirmed red, and restored (see the lane report).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { ContactSummary, Collection } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	requestManifestFrom: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue({}),
	getContacts: vi.fn().mockResolvedValue([]),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn(),
	groupsCreateWithMembers: vi.fn(),
	groupsAssign: vi.fn(),
	groupsDelete: vi.fn(),
	groupsUnassign: vi.fn(),
	contactUpdateGroups: vi.fn(),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

// $page stub: the `/browse?peer=<npub>` deep-link selects the peer through selectPeer (the
// q92/q102/q79 pattern). The npub is inlined because vi.mock is hoisted above every const.
vi.mock('$app/stores', () => ({
	page: {
		subscribe: (fn: (v: { url: URL }) => void) => {
			fn({ url: new URL('http://localhost/browse?peer=npub1archarcharcharcharcharcharcharcharcharcharchar') });
			return () => {};
		},
	},
}));

// bech32-safe fixture id (charset excludes 1/b/i/o after the separator); length matches the npub in
// the stubbed $page URL — peerFromQuery matches on the full string.
const AUTHOR_NPUB = 'npub1archarcharcharcharcharcharcharcharcharcharchar';

/** The 16-MB-limit collection: 100,000 items is a LOWER BOUND, five are in the listing. Deliberately
 *  carries NO `truncated` flag — `oversized` alone must be enough to raise the teaser (the field is
 *  optional and rides beside `truncated`; if only one of the two arrives, the preview is still a
 *  preview). */
const OVERSIZED_COL: Collection = {
	slug: 'huge',
	path_alias: 'The Huge Archive',
	item_count: 100_000,
	total_bytes: 0,
	content_types: ['video'],
	tags: [],
	languages: [],
	last_updated: '2026-08-01T00:00:00Z',
	oversized: true,
	total_items: 100_000,
	listing: Array.from({ length: 5 }, (_, i) => ({
		name: `clip-${i}.mkv`,
		item_type: 'File' as const,
		tags: [],
		children: [],
	})),
};

/** The unchanged control: a 16-MB-and-under truncated teaser, exactly as devtest #7 shipped it —
 *  40 items, 5 shown, so "35 more items hidden". */
const TRUNCATED_COL: Collection = {
	slug: 'vault',
	path_alias: 'The Vault',
	item_count: 40,
	total_bytes: 0,
	content_types: ['video'],
	tags: [],
	languages: [],
	last_updated: '2026-08-01T00:00:00Z',
	truncated: true,
	total_items: 40,
	snapshot_fingerprint: 'aaaa',
	teaser_event_id: 'ev-teaser',
	listing: Array.from({ length: 5 }, (_, i) => ({
		name: `clip-${i}.mkv`,
		item_type: 'File' as const,
		tags: [],
		children: [],
	})),
};

const AUTHOR_PEER: ContactSummary = {
	npub: AUTHOR_NPUB,
	has_browse_key: true,
	collections: [OVERSIZED_COL, TRUNCATED_COL],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: { display_name: 'The Author', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

/** Mount, select the peer (deep-link) and open the card whose visible name is `name`. */
async function openCollection(name: string) {
	contacts.set([AUTHOR_PEER]);
	render(BrowsePage);
	await tick();
	await waitFor(() => expect(document.body.textContent).toContain('The Huge Archive'));
	const card = [...document.querySelectorAll<HTMLButtonElement>('.col-card')].find(
		(c) => (c.textContent ?? '').includes(name),
	);
	expect(card, `the "${name}" card must render or this test proves nothing`).toBeTruthy();
	await fireEvent.click(card!);
	await tick();
}

function statusBlock(): HTMLElement | null {
	return document.querySelector('.oversized-status');
}

describe('QURATOR-336 — an oversized collection is shown as a preview of unknown size', () => {
	it('renders the too-large status and the lower-bound count, and never a hidden figure', async () => {
		await openCollection('The Huge Archive');
		await waitFor(() => expect(document.body.textContent).toContain('Too large to send in full'));

		// The footer status: red dot + the two lines, announced, no progress bar.
		const status = statusBlock();
		expect(status, 'the oversized status block must render in the footer row').toBeTruthy();
		expect(status!.getAttribute('role')).toBe('status');
		expect(status!.querySelector('.oversized-dot')).toBeTruthy();
		expect(status!.textContent).toContain('This list is over the 16 MB limit · showing the preview only');
		expect(status!.querySelector('[role="progressbar"]')).toBeNull();

		// The teaser: the count is a LOWER BOUND, and there is no "N more hidden" figure at all.
		expect(document.querySelector('.paywall-title')?.textContent).toBe('Preview of a very large collection');
		expect(document.querySelector('.paywall-sub')?.textContent).toBe('100,000+ items');
		expect(document.body.textContent).not.toContain('more item');
	});

	// mutation: in `routes/browse/+page.svelte`, change the footer guard
	// `{#if selectedCollection?.oversized}` to `{#if false}` → `statusBlock()` is null and this test
	// fails; the truncated-collection test below stays green.
	//
	// mutation: in `routes/browse/+page.svelte`, replace the oversized branch's
	// `<div class="paywall-sub">{paywall.total.toLocaleString()}+ items</div>` with
	// `<div class="paywall-sub">{paywall.hidden.toLocaleString()} more items hidden</div>` → the
	// exact-sub assertion and the `not.toContain('more item')` assertion both fail.

	it('leaves a non-oversized truncated collection exactly as it was', async () => {
		await openCollection('The Vault');
		await waitFor(() => expect(document.body.textContent).toContain('more item'));

		expect(document.querySelector('.paywall-title')?.textContent).toBe('35 more items hidden');
		expect(document.querySelector('.paywall-sub')?.textContent).toBe(
			'Showing 5 of 40. The collection is too large to publish in full.',
		);
		expect(statusBlock(), 'a collection under the 16 MB limit carries no oversized status').toBeNull();
		expect(document.body.textContent).not.toContain('Too large to send in full');
	});

	// mutation: drop `&& !col?.oversized` from `paywallTeaser`'s guard in `lib/browse-view.ts` → the
	// oversized fixture (which carries no `truncated` flag) gets no teaser at all, so `paywall.title`
	// never renders and the first test fails on its exact-title assertion.
});

describe('QURATOR-336 — the status lives in the footer row', () => {
	it('is a child of the metadata-only row, which still names itself on the left', async () => {
		await openCollection('The Huge Archive');
		await waitFor(() => expect(document.body.textContent).toContain('Too large to send in full'));
		const row = document.querySelector('.no-download-note');
		expect(row, 'the metadata footer row must render').toBeTruthy();
		expect(row!.textContent).toContain('Metadata only. Hoardbook moves no files.');
		expect(row!.contains(statusBlock())).toBe(true);
	});

	// mutation: lift the `{#if selectedCollection?.oversized}` block out of `.no-download-note` in
	// `routes/browse/+page.svelte` (render it as a sibling of the row) → `row.contains(statusBlock())`
	// is false while every other test here stays green.
});
