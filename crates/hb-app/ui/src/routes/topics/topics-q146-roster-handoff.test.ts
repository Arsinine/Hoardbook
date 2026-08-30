// @vitest-environment jsdom
// QURATOR-146 (Topics W4) — the roster hand-off: Contacts and Topics are two paths to the same
// end, and a read-only roster dead-ends the interest-first flow. This file pins the THREE new
// behaviours on a real mount (CLAUDE.md §7: mount the page, mock only `$lib/api.js`; the stores
// are set directly, the contract the layout already fulfils in production):
//
//   1. Double-click a roster member → `/chat?peer=<npub>` — the same gesture Contacts uses. Owner
//      ruling: NO add-to-contacts button on the row ("talk to them first, then add them, from the
//      chat page") — pinned here as an absence: `upsert_topic_contact` must never appear.
//   2. Enter on the focused row does exactly what double-click does (keyboard parity).
//   3. Hover a member → their bio, one lazy pasteKey fetch per hovered person, CACHED (a re-hover
//      costs no second relay round-trip), and absent-means-absent: a peer whose resolve carries no
//      bio renders the HONEST "No published profile" line, never a blank card. A REJECTED resolve
//      (GLM review, 2026-08-28) is "couldn't ask", NOT "asked and there is none": it renders no
//      bio line and the next hover retries — the rejection is never cached as final absence.
//
// Per CLAUDE.md §9 / P-10 these were proven red by mutating the production half each pins:
//   - deleting the ondblclick handler reds the double-click test; deleting the Enter keydown reds
//     the keyboard test (and vice versa — each gesture is independently wired, not shared by
//     accident of one handler doing both);
//   - rendering nothing for `bio === false` (instead of the honest line) reds the absent case;
//   - dropping the `npub in rosterBios` cache check reds the no-refetch assertion (pasteKey would
//     fire twice for two hovers).
//
// jsdom computes no layout — nothing here proves the bio expands visually or the hover cue appears
// at the right position; only that the DOM nodes exist and the api calls happen the right number
// of times.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { readFileSync } from 'node:fs';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import { contacts, identity, profile } from '$lib/stores.js';

// `$app/navigation`'s goto is SPA navigation; outside a kit runtime it logs a no-op warning, so we
// mock it and assert the URL it was handed.
const gotoMock = vi.fn();
vi.mock('$app/navigation', () => ({ goto: (...a: unknown[]) => gotoMock(...a) }));

vi.mock('$lib/api.js', () => ({
	pasteKey: vi.fn().mockResolvedValue({ profile: null }),
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn(),
	topicUpdateMeta: vi.fn(),
	topicDiscoverPaint: vi.fn().mockResolvedValue([]),
	topicRank: vi.fn().mockResolvedValue([]),
	topicLookup: vi.fn().mockResolvedValue({ topic_id: '', name: '', exists: false, member_count_estimate: 0 }),
	topicJoinPublic: vi.fn(),
	topicRedeemInvite: vi.fn(),
	topicPreviewInvite: vi.fn(),
	topicLeave: vi.fn(),
	topicInvite: vi.fn(),
	topicRoster: vi.fn().mockResolvedValue([]),
	topicAnnounce: vi.fn(),
	topicAnnounceStatus: vi.fn().mockResolvedValue(0),
}));

import { topicRoster, topicList, pasteKey } from '$lib/api.js';
const rosterMock = topicRoster as unknown as ReturnType<typeof vi.fn>;
const listMock = topicList as unknown as ReturnType<typeof vi.fn>;
const pasteKeyMock = pasteKey as unknown as ReturnType<typeof vi.fn>;

const SELF_NPUB = 'npub1selfselfselfselfselfselfselfselfselfselfselfse';
const STRANGER_NPUB = 'npub1strangerstrangerstrangerstrangerstrange';
const ONE_TOPIC = [{ topic_id: 't1', name: 'video/anime', description: '', tags: [], private: false, joined_at: 0 }];

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	gotoMock.mockClear();
	contacts.set([]);
	identity.set(null);
	profile.set(null);
});

function seedSelf() {
	identity.set({ npub: SELF_NPUB, npub_short: 'npub1sel…lfse', share_code: 'hbk-x', key_storage: 'plain-file' });
	profile.set({ display_name: 'Me', bio: undefined, tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' });
}

/** Click the first My Topics row so the detail pane + roster render, then wait for the roster. */
async function openFirstTopic(container: HTMLElement) {
	await waitFor(() => expect(container.querySelector('.topic-row')).not.toBeNull());
	await fireEvent.click(container.querySelector<HTMLButtonElement>('.topic-row')!);
	await waitFor(() => expect(rosterMock).toHaveBeenCalled());
	await tick();
}

/** The stranger's row — the roster's non-self button. These tests never add a contact for the
 *  stranger; the row must work WITHOUT one (that is the point of the ticket). */
function strangerRow(container: HTMLElement) {
	const row = container.querySelector<HTMLButtonElement>('.roster-row:not(.self)');
	expect(row).toBeTruthy();
	return row!;
}

describe('QURATOR-146 — roster row hands off to chat', () => {
	it('double-click navigates to /chat?peer=<npub> for a NON-contact member', async () => {
		seedSelf();
		rosterMock.mockResolvedValue([STRANGER_NPUB]);
		listMock.mockResolvedValue(ONE_TOPIC);
		const { container } = render(TopicsPage);
		await openFirstTopic(container);

		await fireEvent.dblClick(strangerRow(container));
		expect(gotoMock).toHaveBeenCalledWith('/chat?peer=' + STRANGER_NPUB);
	});

	it('Enter on the focused row does exactly what double-click does', async () => {
		seedSelf();
		rosterMock.mockResolvedValue([STRANGER_NPUB]);
		listMock.mockResolvedValue(ONE_TOPIC);
		const { container } = render(TopicsPage);
		await openFirstTopic(container);

		const row = strangerRow(container);
		row.focus();
		await fireEvent.keyDown(row, { key: 'Enter' });
		expect(gotoMock).toHaveBeenCalledWith('/chat?peer=' + STRANGER_NPUB);
	});

	it('the self row offers no hand-off (you cannot DM yourself)', async () => {
		seedSelf();
		rosterMock.mockResolvedValue([SELF_NPUB]);
		listMock.mockResolvedValue(ONE_TOPIC);
		const { container } = render(TopicsPage);
		await openFirstTopic(container);

		// The self entry renders the plain non-interactive form — no interactive row at all.
		expect(container.querySelector('.roster-row.self')).toBeTruthy();
		expect(container.querySelector('.roster-row:not(.self)')).toBeNull();
	});

	it('hovers render the bio — ONE lazy pasteKey per person, cached across re-hovers', async () => {
		seedSelf();
		rosterMock.mockResolvedValue([STRANGER_NPUB]);
		listMock.mockResolvedValue(ONE_TOPIC);
		pasteKeyMock.mockResolvedValue({
			npub: STRANGER_NPUB,
			profile: { display_name: 'Stranger', bio: 'I collect laserdisc rips.', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
			collections: [], online: false, last_fetched: '',
		});
		const { container, findByText } = render(TopicsPage);
		await openFirstTopic(container);

		const row = strangerRow(container);
		await fireEvent.mouseEnter(row);
		expect(await findByText('I collect laserdisc rips.')).toBeTruthy();

		// Re-hover (after the fetch settled) must NOT refetch — the cache answers, never a sweep.
		await fireEvent.mouseLeave(row);
		await fireEvent.mouseEnter(row);
		await tick();
		expect(pasteKeyMock).toHaveBeenCalledTimes(1);
	});

	it('an absent profile renders the honest "No published profile" line — never blank, never an empty card', async () => {
		seedSelf();
		rosterMock.mockResolvedValue([STRANGER_NPUB]);
		listMock.mockResolvedValue(ONE_TOPIC);
		// pasteKey resolves a peer whose profile carries no bio — "asked and there is none".
		pasteKeyMock.mockResolvedValue({
			npub: STRANGER_NPUB,
			profile: { display_name: 'Stranger', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
			collections: [], online: false, last_fetched: '',
		});
		const { container, findByText } = render(TopicsPage);
		await openFirstTopic(container);

		await fireEvent.mouseEnter(strangerRow(container));

		// The stated nothing, not an empty tooltip div.
		expect(await findByText('No published profile')).toBeTruthy();
		expect(container.querySelectorAll('.roster-bio')).toHaveLength(1);
	});

	it('a pasteKey REJECTION renders no bio line and a later hover retries (GLM review: a reject is not "asked and there is none")', async () => {
		seedSelf();
		rosterMock.mockResolvedValue([STRANGER_NPUB]);
		listMock.mockResolvedValue(ONE_TOPIC);
		pasteKeyMock.mockRejectedValueOnce(new Error('relay unreachable')).mockResolvedValue({
			npub: STRANGER_NPUB,
			profile: { display_name: 'Stranger', bio: 'back online', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
			collections: [], online: false, last_fetched: '',
		});
		const { container, findByText } = render(TopicsPage);
		await openFirstTopic(container);

		await fireEvent.mouseEnter(strangerRow(container));
		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 20));
		// "Couldn't ask" must NOT assert the bio absent for the session — the stated-nothing line
		// is reserved for a resolve that genuinely carried no bio.
		expect(container.querySelector('.roster-bio')).toBeNull();

		// And a later hover retries: the relay is back and the real bio lands.
		await fireEvent.mouseLeave(strangerRow(container));
		await fireEvent.mouseEnter(strangerRow(container));
		expect(await findByText('back online')).toBeTruthy();
		expect(pasteKeyMock).toHaveBeenCalledTimes(2);
	});

	it('never wires upsert_topic_contact — no add-to-contacts affordance on the roster (owner ruling)', () => {
		// P-12: what must never appear is the AFFORDANCE, not the word — the page's own comment
		// explaining this very ruling names the symbol, so strip comments before asserting absence
		// (a raw whole-file match would red on our own prose). (process.cwd() is crates/hb-app/ui
		// under vitest; import.meta.url is http:// under jsdom.)
		const src = readFileSync(process.cwd() + '/src/routes/topics/+page.svelte', 'utf8');
		const noComments = src
			.replace(/\/\*[\s\S]*?\*\//g, '')   // block comments
			.replace(/(^|\s)\/\/[^\n]*/g, '$1') // line comments
			.replace(/<!--[\s\S]*?-->/g, '');   // HTML comments
		expect(noComments).not.toContain('upsert_topic_contact');
	});
});
