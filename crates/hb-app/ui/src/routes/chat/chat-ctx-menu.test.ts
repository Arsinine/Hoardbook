// @vitest-environment jsdom
// QURATOR-381 — right-click context menu on Chat conversation rows (peers only). The menu carries
// ONLY actions that already exist somewhere on the chat page for that peer: View profile (the pane
// header's viewProfile → /contacts), Browse (the Contacts deep-link, gated on public collections —
// the half of the Contacts row gate this page has data for) and Block (the pane header's
// handleBlockPeer through the same two-step ConfirmButton — blocking is irreversible, so a stray
// first click reveals the consequence, and no copy may promise an access revocation, §6). Opening
// the menu SELECTS the conversation first (Block reads the open selection). Topic rows and the
// Requests row get NO menu: the chat page has no actions for them to mirror (topic management
// lives on the Topics page) — "don't invent new ones".
//
// Mounted (P-4): the `$lib/api.js` mock drives the roster — getMessages returns one message from
// the contact so the peer's row renders (messageless contacts leave the list). Placement
// arithmetic lives in OverflowMenu.test.ts (jsdom computes no layout — P-13).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent, createEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts } from '$lib/stores.js';
import { page as pageStore } from '$app/stores';
import type { Writable } from 'svelte/store';
import type { ContactSummary, Collection } from '$lib/types.js';
const page = pageStore as unknown as Writable<{ url: URL }>;

const { ME, CONTACT } = vi.hoisted(() => ({
	ME: 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
	CONTACT: 'npub1contacttttttttttttttttttttttttttttttttttttttttttttttttttt',
}));

// $app/stores: outside a real SvelteKit navigation context `page` is undefined in the page's
// $effect — stub a WRITABLE store, the chat-q339 pattern.
const stubPage = vi.hoisted(async () => {
	const { writable } = await import('svelte/store');
	return { page: writable({ url: new URL('http://localhost/chat') }) };
});
vi.mock('$app/stores', () => stubPage);

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
import { goto } from '$app/navigation';
const gotoMock = goto as unknown as ReturnType<typeof vi.fn>;

vi.mock('$lib/api.js', () => ({
	getMessages: vi.fn().mockResolvedValue([
		{ from: CONTACT, to: ME, content: 'hello from Dana', sent_at: new Date().toISOString() },
	]),
	sendMessage: vi.fn(),
	sendAccessRequest: vi.fn(),
	grantBrowseAccess: vi.fn(),
	pasteKey: vi.fn().mockResolvedValue(null),
	follow: vi.fn().mockResolvedValue(undefined),
	validateShareCode: vi.fn().mockResolvedValue(null),
	shareCodeInfo: vi.fn().mockResolvedValue(null),
	topicList: vi.fn().mockResolvedValue([]),
	topicChannel: vi.fn().mockResolvedValue({ posts: [], announcements: [] }),
	topicPost: vi.fn(),
	getContacts: vi.fn().mockResolvedValue([]),
	dmRequests: vi.fn().mockResolvedValue([]),
	dmRequestAccept: vi.fn().mockResolvedValue([]),
	dmRequestDecline: vi.fn(),
	dmBlock: vi.fn().mockResolvedValue(undefined),
	unfollowContact: vi.fn().mockResolvedValue(undefined),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn().mockResolvedValue(undefined),
	contactUpdateGroups: vi.fn(),
	advanceReadWatermark: vi.fn().mockResolvedValue(undefined),
	topicAnnounceMarkSeen: vi.fn(),
	getShareCode: vi.fn().mockResolvedValue(''),
	relayStatus: vi.fn().mockResolvedValue([]),
	getCollections: vi.fn().mockResolvedValue([]),
	exportManifest: vi.fn(),
	redeemManifestTicket: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	getSettings: vi.fn().mockResolvedValue({ big_relay_url: '' }),
}));

import { dmBlock } from '$lib/api.js';
const dmBlockMock = dmBlock as unknown as ReturnType<typeof vi.fn>;

const BLOCK_CONFIRM_TEXT = "Block this person? They can't message you and you won't see future requests. You can unblock in Settings.";

/** A contact with DM history plus two public collections (drives the Browse gate). */
function contactFixture(overrides: Partial<ContactSummary> = {}): ContactSummary {
	return {
		npub: CONTACT,
		has_browse_key: false,
		collections: [
			{ slug: 'films', path_alias: 'films', visibility: 'Public' },
			{ slug: 'tapes', path_alias: 'tapes', visibility: 'Public' },
		] as unknown as Collection[],
		online: false,
		last_fetched: new Date().toISOString(),
		local_tags: [],
		...overrides,
	} as unknown as ContactSummary;
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	page.set({ url: new URL('http://localhost/chat') });
});

/** Mount with identity + one contact that has history, wait for its conversation row. */
async function mountWithRow() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	contacts.set([contactFixture()]);
	const ui = render(ChatPage);
	const row = await waitFor(() => {
		const found = ui.container.querySelector<HTMLElement>('button.convo-item');
		expect(found, 'the peer conversation row rendered').not.toBeNull();
		return found!;
	});
	return { ui, row };
}

describe('QURATOR-381 — Chat conversation-row context menu', () => {
	it('right-click opens one menu with View profile, Browse and the two-step Block; the event is default-prevented', async () => {
		const { ui, row } = await mountWithRow();
		const event = createEvent.contextMenu(row, { clientX: 30, clientY: 40 });
		fireEvent(row, event);
		expect(event.defaultPrevented).toBe(true);

		await waitFor(() => expect(ui.container.querySelector('.overflow-menu')).not.toBeNull());
		expect(ui.container.querySelectorAll('.overflow-menu').length).toBe(1);
		const items = [...ui.container.querySelectorAll('.overflow-menu > button.menu-item')].map((b) => (b as HTMLElement).textContent?.trim());
		expect(items).toEqual(['View profile', 'Browse']);
		expect(ui.container.querySelector('.overflow-menu .menu-item-confirm')?.textContent).toContain('Block');
	});

	it('View profile calls the same navigation the pane header does (goto /contacts) and closes the menu', async () => {
		const { ui, row } = await mountWithRow();
		fireEvent.contextMenu(row, { clientX: 10, clientY: 10 });
		const menu = await waitFor(() => ui.container.querySelector('.overflow-menu') as HTMLElement);
		await fireEvent.click([...menu.querySelectorAll('button.menu-item')].find((b) => b.textContent?.trim() === 'View profile') as HTMLElement);
		await tick();
		expect(gotoMock).toHaveBeenCalledWith('/contacts');
		expect(ui.container.querySelector('.overflow-menu'), 'the menu closed behind the navigation').toBeNull();
	});

	// P-10 MUTATION (seen RED, then restored): point the menu's Browse at a DIFFERENT target —
	// `'/browse?peer=' + …` → `'/contacts'` — reds this test.
	it('Browse deep-links to the peer exactly as the Contacts row does (goto /browse?peer=<npub>)', async () => {
		const { ui, row } = await mountWithRow();
		fireEvent.contextMenu(row, { clientX: 10, clientY: 10 });
		const menu = await waitFor(() => ui.container.querySelector('.overflow-menu') as HTMLElement);
		await fireEvent.click([...menu.querySelectorAll('button.menu-item')].find((b) => b.textContent?.trim() === 'Browse') as HTMLElement);
		await tick();
		expect(gotoMock).toHaveBeenCalledWith('/browse?peer=' + CONTACT);
	});

	it('Block is the two-step confirm: first click reveals the consequence and fires nothing, Confirm fires dm_block once', async () => {
		const { ui, row } = await mountWithRow();
		fireEvent.contextMenu(row, { clientX: 10, clientY: 10 });
		await waitFor(() => expect(ui.container.querySelector('.overflow-menu')).not.toBeNull());
		const confirmHost = ui.container.querySelector('.overflow-menu .menu-item-confirm') as HTMLElement;

		const blockBtn = confirmHost.querySelector('button') as HTMLElement;
		await fireEvent.click(blockBtn);
		await tick();
		expect(dmBlockMock).not.toHaveBeenCalled();
		expect(confirmHost.textContent).toContain(BLOCK_CONFIRM_TEXT);

		await fireEvent.click([...confirmHost.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Confirm') as HTMLElement);
		await waitFor(() => expect(dmBlockMock).toHaveBeenCalledTimes(1));
		expect(dmBlockMock).toHaveBeenCalledWith(CONTACT);
	});

	// P-10 MUTATION (seen RED, then restored): drop the public-collections gate — render Browse
	// unconditionally — reds this test (the collection-less row grows a Browse).
	it('a peer with no collections shows NO Browse in the context menu', async () => {
		contacts.set([contactFixture({ collections: [] })]);
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		const ui = render(ChatPage);
		const row = await waitFor(() => {
			const found = ui.container.querySelector('button.convo-item');
			expect(found).not.toBeNull();
			return found!;
		});
		fireEvent.contextMenu(row, { clientX: 10, clientY: 10 });
		await waitFor(() => expect(ui.container.querySelector('.overflow-menu')).not.toBeNull());
		const items = [...ui.container.querySelectorAll('.overflow-menu > button.menu-item')].map((b) => (b as HTMLElement).textContent?.trim());
		expect(items).not.toContain('Browse');
		expect(items).toContain('View profile');
	});

	// P-10 MUTATION (seen RED, then restored): drop the F10/ContextMenu branch from the row's
	// onkeydown — reds this test.
	it('Shift+F10 on a focused row opens the same menu', async () => {
		const { ui, row } = await mountWithRow();
		row.focus();
		await fireEvent.keyDown(row, { key: 'F10', shiftKey: true });
		await waitFor(() => expect(ui.container.querySelector('.overflow-menu')).not.toBeNull());
		await fireEvent.keyDown(document, { key: 'Escape' });
		await tick();
		expect(ui.container.querySelector('.overflow-menu'), 'Escape closes (OverflowMenu owns it)').toBeNull();
	});
});
