// @vitest-environment jsdom
// QURATOR-304 — liveness (a 30-day presence window) governs membership on every surface except
// the roster panel: `topic_rank`'s alive_count ignores a member with no beacon in the window and
// the directory DROPS dead Topics, but `topic_roster` listed everyone identically, so a member
// silent for years rendered as a normal row while every count correctly ignored them. Owner
// 2026-09-20: absence is decay, and the roster is the member's own room — so the row is KEPT and
// DIMMED (shown-dormant), the header STATES the split instead of silently disagreeing with the
// alive count, and an UNKNOWN read (dormant === null, the presence fetch failed) renders normally.
//
// This file pins, on a real mount (CLAUDE.md §7: mount the page, mock only `$lib/api.js`):
//   1. a dormant member's row carries `.dormant` AND the "silent 30d+" TEXT cue (distinguishable
//      without a tooltip); the SELF row never dims even when its own beacon read says dormant;
//   2. the header states the split — `Roster (2 · 1 recent)` — the same 30-day fold the
//      directory's alive count applies, so the panel AGREES with the count instead of merely
//      totalling bodies; with liveness unknown it falls back to the plain `Roster (2)`;
//   3. dormancy COMPOSES with the QURATOR-142 opt-out, it never overrides it: a dormant
//      opted-IN member still unlocks to the clickable button form (dormancy does not lock the
//      hand-off); a dormant opted-OUT member stays the locked non-clickable div — still dimmed;
//   4. fail-closed survives: before the pasteKey resolve lands, a dormant stranger renders the
//      locked form (unresolved opt-out reads as opted OUT, dormant or not).
//
// Per CLAUDE.md §9 / P-10 these were proven red by mutating the production half each pins
// (anchors are LINE NUMBERS in +page.svelte — re-grep before re-proving, the file moves):
//   - `{@const dormant = ...}` → `false` reds tests 1, 3 (no `.dormant` row, no cue);
//   - `rosterHeaderSuffix`'s condition → always `''` reds test 2's split header;
//   - adding `|| dormant` to the `{#if row.isSelf || rosterChatLocked(m.npub)}` branch condition
//     reds test 3's unlock (a dormant member would NEVER become clickable).
//
// jsdom computes no layout — nothing here proves the row RENDERS dimmed (opacity 0.55 lives in
// CSS); only that the class, the cue text, and the element forms are right.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import { contacts, identity, profile } from '$lib/stores.js';

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

/** Roster rows in the NEW `topic_roster` shape (QURATOR-304): npub + dormant tri-state. */
const member = (npub: string, dormant: boolean | null) => ({ npub, dormant });

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

async function openFirstTopic(container: HTMLElement) {
	await waitFor(() => expect(container.querySelector('.topic-row')).not.toBeNull());
	await fireEvent.click(container.querySelector<HTMLButtonElement>('.topic-row')!);
	await waitFor(() => expect(rosterMock).toHaveBeenCalled());
	await tick();
}

/** The roster's `<li>` wrappers, in roster order (the page renders the fetch order, no sort). */
function rosterItems(container: HTMLElement) {
	const items = Array.from(container.querySelectorAll('.roster-item'));
	expect(items.length).toBeGreaterThan(0);
	return items;
}

/** The clickable/locked row element (div or button) inside a roster `<li>`. */
function rowOf(li: Element) {
	const row = li.querySelector('.roster-row');
	expect(row).toBeTruthy();
	return row!;
}

function rosterHeader(container: HTMLElement) {
	const label = Array.from(container.querySelectorAll('.section-label')).find((el) =>
		el.textContent?.startsWith('Roster')
	);
	expect(label).toBeTruthy();
	return label!.textContent ?? '';
}

/** A pasteKey resolve carrying the QURATOR-142 opt-out verdict (`hide_in_rosters`). */
function resolvedPeer(hideInRosters: boolean) {
	return {
		npub: STRANGER_NPUB,
		profile: { display_name: 'Stranger', bio: 'b', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], hide_in_rosters: hideInRosters, updated: '' },
		collections: [], online: false, last_fetched: '',
	};
}

describe('QURATOR-304 — dormant roster members are shown dimmed, never dropped', () => {
	it('a dormant member keeps their row, carries .dormant and the "silent 30d+" cue; self never dims', async () => {
		seedSelf();
		// SELF is deliberately ALSO marked dormant: the self row must never dim (you are online
		// by construction — you are reading the panel), pinning the `!row.isSelf` guard.
		rosterMock.mockResolvedValue([member(STRANGER_NPUB, true), member(SELF_NPUB, true)]);
		listMock.mockResolvedValue(ONE_TOPIC);
		const { container } = render(TopicsPage);
		await openFirstTopic(container);

		const [strangerItem, selfItem] = rosterItems(container);
		const strangerRow = rowOf(strangerItem);
		expect(strangerRow.classList.contains('dormant')).toBe(true);
		expect(strangerItem.textContent).toContain('silent 30d+');

		const selfRow = rowOf(selfItem);
		expect(selfRow.classList.contains('self')).toBe(true);
		expect(selfRow.classList.contains('dormant')).toBe(false);
		// The dormant row is still LISTED — shown-dormant, not hidden.
		expect(container.querySelectorAll('.roster-item').length).toBe(2);
	});

	it('the header states the split the directory\'s alive count would show, and falls back when unknown', async () => {
		seedSelf();
		rosterMock.mockResolvedValue([member(SELF_NPUB, false), member(STRANGER_NPUB, true)]);
		listMock.mockResolvedValue(ONE_TOPIC);
		const { container } = render(TopicsPage);
		await openFirstTopic(container);
		// 2 members, 1 with a beacon in the window — the panel AGREES with alive_count=1 instead
		// of silently disagreeing.
		expect(rosterHeader(container)).toBe('Roster (2 · 1 recent)');

		// Unknown liveness (the presence read failed): plain total, no dormant rows, still listed.
		rosterMock.mockResolvedValue([member(SELF_NPUB, false), member(STRANGER_NPUB, null)]);
		const again = render(TopicsPage);
		await openFirstTopic(again.container);
		expect(rosterHeader(again.container)).toBe('Roster (2)');
		expect(again.container.querySelector('.roster-row.dormant')).toBeNull();
		expect(again.container.querySelectorAll('.roster-item').length).toBe(2);

		// All recent: also the plain total (no spurious split).
		rosterMock.mockResolvedValue([member(SELF_NPUB, false), member(STRANGER_NPUB, false)]);
		const third = render(TopicsPage);
		await openFirstTopic(third.container);
		expect(rosterHeader(third.container)).toBe('Roster (2)');
	});

	it('a dormant opted-IN member still unlocks to the clickable button — dormancy does not lock the hand-off', async () => {
		seedSelf();
		rosterMock.mockResolvedValue([member(STRANGER_NPUB, true), member(SELF_NPUB, false)]);
		listMock.mockResolvedValue(ONE_TOPIC);
		pasteKeyMock.mockResolvedValue(resolvedPeer(false));
		const { container } = render(TopicsPage);
		await openFirstTopic(container);

		// Fail-closed first: unresolved opt-out reads as opted OUT even for a dormant member.
		const [strangerItem] = rosterItems(container);
		let strangerRow = rowOf(strangerItem);
		expect(strangerRow.tagName).toBe('DIV');

		// The hover resolve says hide_in_rosters:false — the dormant row unlocks to a BUTTON,
		// KEEPS the dormant class, and the hand-off still fires.
		await fireEvent.mouseEnter(strangerRow);
		await waitFor(() => {
			const row = rowOf(rosterItems(container)[0]);
			expect(row.tagName).toBe('BUTTON');
			expect(row.classList.contains('dormant')).toBe(true);
		});
		await fireEvent.dblClick(rowOf(rosterItems(container)[0]));
		expect(gotoMock).toHaveBeenCalledWith('/chat?peer=' + STRANGER_NPUB);
	});

	it('a dormant opted-OUT member stays the locked non-clickable div — still dimmed (composition)', async () => {
		seedSelf();
		rosterMock.mockResolvedValue([member(STRANGER_NPUB, true), member(SELF_NPUB, false)]);
		listMock.mockResolvedValue(ONE_TOPIC);
		pasteKeyMock.mockResolvedValue(resolvedPeer(true));
		const { container } = render(TopicsPage);
		await openFirstTopic(container);

		const [strangerItem] = rosterItems(container);
		await fireEvent.mouseEnter(rowOf(strangerItem));
		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalled());
		await tick();

		// Opt-out keeps the locked div form (QURATOR-142), and dormancy composes on top of it:
		// dimmed + cued, never clickable, never dropped.
		const strangerRow = rowOf(rosterItems(container)[0]);
		expect(strangerRow.tagName).toBe('DIV');
		expect(strangerRow.classList.contains('dormant')).toBe(true);
		expect(strangerItem.textContent).toContain('silent 30d+');
		await fireEvent.dblClick(strangerRow);
		expect(gotoMock).not.toHaveBeenCalled();
	});
});
