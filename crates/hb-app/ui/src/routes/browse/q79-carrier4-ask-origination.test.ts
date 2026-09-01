// @vitest-environment jsdom
// QURATOR-79 carrier 4 — the ASK-ORIGINATION slice. The C-side (serving) and the inbox-recognition
// side had landed; D had no way to originate the ask. This pins the Browse paywall's new affordance:
// the user picks a contact (peer C) and the ask names the collection's AUTHOR (peer A — the browsed
// peer), through `request_manifest_from`.
//
// This is a BEHAVIOURAL mount test (the q83/q79-provenance pattern): the real Browse page is
// mounted with only `$lib/api.js` mocked, the peer is selected through the `/browse?peer=` deep-link
// (which routes through selectPeer exactly as production does), the truncated collection is clicked
// open, and the ask is driven through the same buttons a user presses. A source-scan is NOT
// acceptable for this (CLAUDE.md → P-4): the property is what the page CALLS and with WHICH author,
// which only a mount can see.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red. The mutation probes (each applied
// alone, then reverted) are documented in the lane report:
//   A. in `handleAskContact`, swap the `requestManifestFrom(...)` call for `requestManifest(...)`
//      (dropping the author) — reds the "calls the command with the author's npub" assertions in
//      every test here; the affordance-presence assertion alone would stay green, which is why the
//      call-shape asserts exist.
//   B. in the template, delete the `Ask a contact for this list` button — reds every test at the
//      affordance-presence step.
//   C. in `askableContacts`, drop the `c.npub !== selectedPeer.npub` filter — reds the
//      "author is never offered" test only.
//
// jsdom computes no layout — nothing here proves the picker row RENDERS on one line; only that the
// affordance appears and that invoking it calls the command with the author's npub.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts, toastMessage } from '$lib/stores.js';
import type { CachedPeer, Collection } from '$lib/types.js';

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
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse?peer=npub1archarcharcharcharcharcharcharcharcharcharchar') }) };
});
vi.mock('$app/stores', () => stubPage);

// bech32-safe fixture ids (charset excludes 1/b/i/o after the separator); length matches the npub
// in the stubbed $page URL — peerFromQuery matches on the full string.
const AUTHOR_NPUB = 'npub1archarcharcharcharcharcharcharcharcharcharchar';
// Peer C: the contact D asks. A CONTACT, so the picker must offer them by display name.
const CONTACT_NPUB = 'npub1m1ram1ram1ram1ram1ram1ram1ram1ram1ram1ram1r';

const TRUNCATED_COL: Collection = {
	slug: 'archive',
	path_alias: 'The Archive',
	item_count: 40,
	total_bytes: 0,
	content_types: ['video'],
	tags: [],
	languages: [],
	last_updated: '2026-08-01T00:00:00Z',
	// devtest #7 paywall teaser: truncated + total_items > what the listing carries. The paywall
	// block — where the ask affordance lives — only renders for a truncated collection.
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

const AUTHOR_PEER: CachedPeer = {
	npub: AUTHOR_NPUB,
	browse_key_hex: 'aabbccdd',
	collections: [TRUNCATED_COL],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: { display_name: 'The Author', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
};

const CONTACT_PEER: CachedPeer = {
	npub: CONTACT_NPUB,
	collections: [],
	online: true,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: { display_name: 'Mira', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	toastMessage.set(null);
});

import { requestManifestFrom, requestManifest } from '$lib/api.js';
const askFromMock = requestManifestFrom as unknown as ReturnType<typeof vi.fn>;
const askOwnerMock = requestManifest as unknown as ReturnType<typeof vi.fn>;

/** Drive the real page to the paywall block with the contact picker OPEN, and return the picker. */
async function driveToOpenPicker(): Promise<HTMLSelectElement> {
	contacts.set([AUTHOR_PEER, CONTACT_PEER]);
	render(BrowsePage);
	await tick();

	// Open the truncated collection — the paywall block with the ask affordances is inside.
	await waitFor(() => expect(document.body.textContent).toContain('The Archive'));
	const card = document.querySelector<HTMLButtonElement>('.col-card');
	expect(card).toBeTruthy();
	await fireEvent.click(card!);
	await tick();
	await waitFor(() => expect(document.body.textContent).toContain('more item'));

	// The ask-a-contact affordance — the button a user presses.
	const openBtn = [...document.querySelectorAll('button')].find(
		(b) => b.textContent?.trim() === 'Ask a contact for this list',
	);
	expect(openBtn, 'the ask-a-contact affordance must appear in the paywall block').toBeTruthy();
	await fireEvent.click(openBtn!);
	await tick();

	const select = document.querySelector<HTMLSelectElement>('.ask-contact-select');
	expect(select, 'opening the affordance reveals the contact picker').toBeTruthy();
	return select!;
}

describe('QURATOR-79 carrier 4 — ask origination (D asks C for A\'s manifest)', () => {
	it('the affordance appears in the paywall block and offers contacts BY NAME, excluding the author', async () => {
		const select = await driveToOpenPicker();
		const options = [...select.querySelectorAll('option')];
		// Mira is offered by display name — the "by name" half of design §5's sentence.
		expect(options.some((o) => o.textContent?.trim() === 'Mira' && o.value === CONTACT_NPUB)).toBe(true);
		// The author is NEVER offered: asking A directly is the "Ask the owner" button beside it.
		expect(options.every((o) => o.value !== AUTHOR_NPUB)).toBe(true);
		// The honest uncertainty copy is present.
		expect(document.body.textContent).toContain('They’ll only see this if they hold a copy');
	});

	it('invoking it calls request_manifest_from with the AUTHOR\'s npub (never the contact\'s)', async () => {
		const select = await driveToOpenPicker();
		await fireEvent.change(select, { target: { value: CONTACT_NPUB } });
		await tick();

		const askBtn = [...document.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Ask them');
		expect(askBtn, 'choosing a contact enables the Ask them button').toBeTruthy();
		expect((askBtn as HTMLButtonElement).disabled).toBe(false);
		await fireEvent.click(askBtn!);

		await waitFor(() => expect(askFromMock).toHaveBeenCalledTimes(1));
		// THE assertion that matters: the ask names WHO AUTHORED the collection (the browsed peer A),
		// and is ADDRESSED to the chosen contact C. The two must not be swapped or dropped.
		expect(askFromMock).toHaveBeenCalledWith(
			CONTACT_NPUB,
			AUTHOR_NPUB,
			'archive',
			'aaaa',
			'ev-teaser',
		);
		// The owner-path ask is NOT fired by this affordance.
		expect(askOwnerMock).not.toHaveBeenCalled();
		// Success is fed back: the toast names the asked CONTACT (by name), never the raw npub.
		const { get } = await import('svelte/store');
		await waitFor(() => expect(get(toastMessage)).not.toBeNull());
		expect(get(toastMessage)?.text).toContain('Mira');
		expect(get(toastMessage)?.text).not.toContain(CONTACT_NPUB);
	});

	it('the ask is re-read from the persisted map after the send (the trace is not optimistic)', async () => {
		const { getManifestAsks } = await import('$lib/api.js');
		const asksMock = getManifestAsks as unknown as ReturnType<typeof vi.fn>;
		const select = await driveToOpenPicker();
		const callsBefore = asksMock.mock.calls.length;
		await fireEvent.change(select, { target: { value: CONTACT_NPUB } });
		await tick();
		const askBtn = [...document.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Ask them');
		await fireEvent.click(askBtn!);
		// A successful send re-reads the persisted map so the asked-state renders from the store —
		// the same W7.1a discipline as the owner-path ask.
		await waitFor(() => expect(asksMock.mock.calls.length).toBeGreaterThan(callsBefore));
	});

	it('the honest copy promises nothing about whether the contact holds the list', async () => {
		await driveToOpenPicker();
		const hint = document.querySelector('.ask-contact-hint');
		expect(hint, 'the hint line renders beside the picker').toBeTruthy();
		const text = hint?.textContent ?? '';
		// No promise of possession, no "has"/"holds a copy" certainty directed at the reader.
		expect(text).toContain('Nothing is promised');
		expect(text).toContain('may not have it');
		// MAS-INV-5 unchanged: the NEW copy introduces no "download" (the page's pre-existing
		// "No downloads here" footer is out of scope here — it is not this affordance's copy).
		expect(/download/i.test(text)).toBe(false);
	});
});
