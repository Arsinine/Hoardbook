// @vitest-environment jsdom
// Devtest 2026-08-26 item 5 — owner: "In browse and chat the title bar on the top does not extend
// all the way horizontally across meaning whenever the user are on these two pages they cannot drag
// move the app." Ruling on the artifact: Option B (one unconditional full-width header per page),
// "But make it consistent with the other pages like Home and Contacts. Dont make it so that it
// stands out" — hence the SAME .topbar element/classes those routes use, not a bespoke bar.
//
// These are MOUNT tests, not source scans. The defect is a header that exists in the file but not
// on screen: Chat's four .pane-headers are each behind an {#if}, and with nothing selected none of
// them render — a source scan sees four drag regions and calls it covered. Only a render can tell
// the difference, and §9 of CLAUDE.md is explicit that "we source-scan because it cannot mount" is
// not a justification here.
//
// Mutation probes run against PRODUCTION for this file, one change at a time, each reverted and
// the revert diff-verified against a backup (2026-08-27):
//   a) delete the <div class="topbar"> block from chat/+page.svelte
//        -> both Chat tests red, Browse green.       (2 failed | 1 passed)
//   b) wrap Chat's topbar in {#if $identity} … {/if}
//        -> ONLY the no-identity test red.           (1 failed | 2 passed)
//      That is the whole point of Option B's placement: a header that renders in the common case
//      and vanishes in an empty state is the bug, and (b) is the probe that can tell those apart.
//   c) delete the <div class="topbar"> block from browse/+page.svelte
//        -> ONLY the Browse test red.                (1 failed | 2 passed)
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/svelte';
import BrowsePage from './browse/+page.svelte';
import ChatPage from './chat/+page.svelte';
import { contacts, identity, inboxMessages, sentMessages } from '$lib/stores.js';

const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/') }) };
});
vi.mock('$app/stores', () => stubPage);
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));

// One registry entry covers both pages — the union of what Browse and Chat import.
vi.mock('$lib/api.js', () => ({
	// Browse
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
	groupsCreateWithMembers: vi.fn(),
	groupsAssign: vi.fn(),
	groupsDelete: vi.fn(),
	groupsUnassign: vi.fn(),
	// Chat
	getMessages: vi.fn().mockResolvedValue([]),
	sendMessage: vi.fn(),
	pasteKey: vi.fn().mockResolvedValue({ profile: null }),
	follow: vi.fn().mockResolvedValue(undefined),
	validateShareCode: vi.fn().mockResolvedValue(null),
	shareCodeInfo: vi.fn().mockResolvedValue(null),
	topicList: vi.fn().mockResolvedValue([]),
	topicChannel: vi.fn().mockResolvedValue([]),
	topicPost: vi.fn(),
	dmRequests: vi.fn().mockResolvedValue([]),
	dmRequestAccept: vi.fn(),
	dmRequestDecline: vi.fn(),
	dmBlock: vi.fn().mockResolvedValue(undefined),
	advanceReadWatermark: vi.fn().mockResolvedValue(undefined),
	topicAnnounceMarkSeen: vi.fn(),
	getShareCode: vi.fn().mockResolvedValue(''),
	relayStatus: vi.fn().mockResolvedValue([]),
	exportManifest: vi.fn(),
	sendFullList: vi.fn(),
	redeemManifestTicket: vi.fn(),
	getSettings: vi.fn().mockResolvedValue({ big_relay_url: '' }),
	// shared
	getContacts: vi.fn().mockResolvedValue([]),
	getCollections: vi.fn().mockResolvedValue([]),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn().mockResolvedValue(undefined),
	contactUpdateGroups: vi.fn(),
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	identity.set(null);
	inboxMessages.set([]);
	sentMessages.set([]);
});

/** The one thing every route's drag handle must satisfy: a .topbar that is actually in the document
 *  and actually carries Tauri's drag trigger. Returns it so callers can assert on placement. */
async function theDragBar(): Promise<HTMLElement> {
	const bars = await waitFor(() => {
		const found = document.body.querySelectorAll<HTMLElement>('.topbar');
		expect(found.length, 'no .topbar rendered').toBeGreaterThan(0);
		return found;
	});
	// Exactly one: two would mean a per-branch copy, which is how the conditional-header bug returns.
	expect(bars.length, 'more than one .topbar rendered').toBe(1);
	const bar = bars[0];
	expect(
		bar.hasAttribute('data-tauri-drag-region'),
		'.topbar rendered but carries no data-tauri-drag-region — it is not a drag handle',
	).toBe(true);
	return bar;
}

describe('devtest item 5 — Browse and Chat have a full-width drag bar that always renders', () => {
	it('Browse renders one drag topbar with no peer selected, above the split', async () => {
		render(BrowsePage);
		const bar = await theDragBar();
		// Above the split, not inside it: a bar nested in .browse-shell would be constrained to one
		// column and would not span the window, which is the owner's actual complaint.
		expect(bar.closest('.browse-shell'), '.topbar is nested inside .browse-shell').toBeNull();
		expect(document.body.querySelector('.browse-shell'), 'browse-shell missing').not.toBeNull();
	});

	it('Chat renders one drag topbar with no conversation selected — when NO pane-header exists', async () => {
		identity.set({ npub: 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee' } as never);
		render(ChatPage);
		const bar = await theDragBar();
		expect(bar.closest('.chat-frame'), '.topbar is nested inside .chat-frame').toBeNull();
		// This is the bug, pinned: in the default state Chat's four .pane-header drag regions are all
		// behind an {#if} and none of them render, so before item 5 there was no full-width handle at
		// all. If a future change makes a pane-header unconditional, this assertion should be
		// reconsidered deliberately — not deleted to make the file green.
		expect(document.body.querySelectorAll('.pane-header').length).toBe(0);
	});

	it('Chat renders the drag topbar in the no-identity empty state too', async () => {
		identity.set(null);
		render(ChatPage);
		const bar = await theDragBar();
		// The empty state replaces the entire .chat-frame, so a bar placed inside the {:else} branch
		// would leave this screen undraggable. It sits above the branch for exactly this reason.
		expect(document.body.querySelector('.chat-frame'), 'chat-frame should not render').toBeNull();
		expect(document.body.querySelector('.no-identity'), 'no-identity branch missing').not.toBeNull();
		expect(bar.closest('.no-identity')).toBeNull();
	});
});
