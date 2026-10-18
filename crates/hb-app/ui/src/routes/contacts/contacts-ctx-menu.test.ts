// @vitest-environment jsdom
// QURATOR-381 — right-click context menus on Contacts rows. The row's oncontextmenu preventDefaults
// and opens ONE menu at the cursor: Browse (only when the row would render its inline Browse — the
// same hasCollections predicate), Message, a separator, then every ⋯ menu item with the SAME
// handlers (no re-implementations, so the two cannot drift). Right-click SELECTS the contact first
// (the pane holds the open instance "Edit groups…" anchors from), and the inline buttons + the ⋯
// stay (this menu is an additional path, not a replacement).
//
// Mounted, not a source-scan (P-4): only `$lib/api.js` is mocked, the roster is seeded straight
// into the `contacts` store, assertions target the RENDERED DOM. Placement arithmetic is covered
// by OverflowMenu.test.ts (jsdom computes no layout — P-13); the layout-suppressor ordering is
// proven by the document-level listener twin below.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent, createEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ContactsPage from './+page.svelte';
import { contacts } from '$lib/stores.js';
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
	onlineCount: vi.fn().mockResolvedValue({ online: 0, fetched_at: new Date().toISOString(), relay_set: [], fresh: [] }),
	relayStatus: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	privateAudienceList: vi.fn().mockResolvedValue([]),
	privateAudienceSet: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import { goto } from '$app/navigation';
const gotoMock = goto as unknown as ReturnType<typeof vi.fn>;

const NPUB = 'npub1ctx' + 'c'.repeat(50);
const BIO = 'Keeps long-form cinema notes and trades silent-era reels.';
const PROF: Profile = {
	display_name: 'Ctx Dana',
	bio: BIO,
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: [],
	total_bytes: 5_000_000,
	updated: '2026-08-01T00:00:00Z',
} as unknown as Profile;

function peer(overrides: Partial<ContactSummary> = {}): ContactSummary {
	return {
		npub: NPUB,
		has_browse_key: false,
		collections: [
			{ slug: 'films', path_alias: 'films', visibility: 'Public' },
			{ slug: 'tapes', path_alias: 'tapes', visibility: 'Public' },
		] as unknown as Collection[],
		online: false,
		last_fetched: new Date().toISOString(),
		local_tags: [],
		profile: PROF,
		...overrides,
	};
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

/** The +layout.svelte suppressor twin: document-level, bubble phase, preventDefault-only — the
 *  exact shape of the global that must NOT swallow the row's own handler. */
function layoutTwin(onSeen: (e: MouseEvent) => void) {
	const fn = (e: MouseEvent) => {
		onSeen(e);
		e.preventDefault();
	};
	document.addEventListener('contextmenu', fn);
	return () => document.removeEventListener('contextmenu', fn);
}

describe('QURATOR-381 — Contacts row context menu', () => {
	it('right-click opens one menu with Browse, Message, a separator, then every ⋯ item in order; the event is default-prevented; the contact is selected first', async () => {
		contacts.set([peer()]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => {
			const found = container.querySelector('.contact-card');
			expect(found, 'the contact row rendered').not.toBeNull();
			return found!;
		});

		// The layout twin proves ORDER: by the time the document-level suppressor sees the event,
		// the row's handler has already preventDefaulted it (the row is not swallowed).
		let twinSawDefaultPrevented = false;
		const removeTwin = layoutTwin((e) => { twinSawDefaultPrevented = e.defaultPrevented; });

		const event = createEvent.contextMenu(row, { clientX: 40, clientY: 60 });
		fireEvent(row, event);
		try {
			expect(event.defaultPrevented, 'the row handler preventDefaults the contextmenu').toBe(true);
			expect(twinSawDefaultPrevented, 'the row handler ran BEFORE the document-level suppressor').toBe(true);

			await waitFor(() => expect(container.querySelector('.overflow-menu')).not.toBeNull());
			// Right-click SELECTS first: the pane holds the open contact (its bio renders).
			await waitFor(() => expect(container.querySelector('.detail-pane .pane-bio')).not.toBeNull());
			expect(container.querySelector('.detail-pane .pane-bio')!.textContent).toContain(BIO);

			// Exactly ONE menu (opening the ctx menu must not leave a ⋯ menu behind — none was open,
			// and the ctx menu is the page's only .overflow-menu right now).
			expect(container.querySelectorAll('.overflow-menu').length).toBe(1);

			// Items, in order. "Refreshing…" only renders mid-refresh; at rest the item is "Refresh".
			const items = [...container.querySelectorAll('.overflow-menu > button.menu-item')].map((b) => (b as HTMLElement).textContent?.trim());
			expect(items).toEqual([
				'Browse',
				'Message',
				'Copy npub',
				'Refresh',
				'Edit groups…',
				'Edit tags…',
			]);
			expect(container.querySelector('.overflow-menu .menu-item-sep'), 'the separator after Message').not.toBeNull();
			expect(container.querySelector('.overflow-menu .menu-item-confirm')?.textContent).toContain('Remove contact');
		} finally {
			removeTwin();
		}
	});

	// P-10 MUTATION (seen RED, then restored): point the menu's Message at a DIFFERENT handler —
	// `goto('/chat?peer=' + ctxPeer.npub)` → `goto('/contacts')` — reds this test (and with it the
	// same-navigation pin). The inline button's handler string is pinned separately by
	// contacts-w1.test.ts, so both paths are held to the same target.
	it('Message calls the same navigation the inline button does (goto /chat?peer=<npub>) and closes the menu', async () => {
		contacts.set([peer()]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => container.querySelector<HTMLElement>('.contact-card')!);
		fireEvent.contextMenu(row, { clientX: 10, clientY: 10 });
		const menu = await waitFor(() => container.querySelector('.overflow-menu') as HTMLElement);
		await fireEvent.click([...menu.querySelectorAll('button.menu-item')].find((b) => b.textContent?.trim() === 'Message') as HTMLElement);
		await tick();
		expect(gotoMock).toHaveBeenCalledWith('/chat?peer=' + NPUB);
		expect(container.querySelector('.overflow-menu'), 'the menu closed behind the navigation').toBeNull();
	});

	// P-10 MUTATION (seen RED, then restored): drop the hasCollections gate — render Browse
	// unconditionally in the ctx menu — reds this test (the collection-less row grows a Browse).
	it('a row with no collections shows NO Browse in the context menu', async () => {
		contacts.set([peer({ collections: [] })]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => container.querySelector<HTMLElement>('.contact-card')!);
		fireEvent.contextMenu(row, { clientX: 10, clientY: 10 });
		await waitFor(() => expect(container.querySelector('.overflow-menu')).not.toBeNull());
		const items = [...container.querySelectorAll('.overflow-menu > button.menu-item')].map((b) => (b as HTMLElement).textContent?.trim());
		expect(items).not.toContain('Browse');
		expect(items).toContain('Message');
		expect(items).toContain('Copy npub');
	});

	// P-10 MUTATION (seen RED, then restored): drop the F10/ContextMenu branch from the row's
	// onkeydown — reds this test (the keystroke opens nothing).
	it('Shift+F10 on a focused row opens the same menu', async () => {
		contacts.set([peer()]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => container.querySelector<HTMLElement>('.contact-card')!);
		row.focus();
		await fireEvent.keyDown(row, { key: 'F10', shiftKey: true });
		await waitFor(() => expect(container.querySelector('.overflow-menu')).not.toBeNull());
		await fireEvent.keyDown(document, { key: 'Escape' });
		await tick();
		expect(container.querySelector('.overflow-menu'), 'Escape closes (OverflowMenu owns it)').toBeNull();
	});

	it('the ContextMenu key opens it too', async () => {
		contacts.set([peer()]);
		const { container } = render(ContactsPage);
		const row = await waitFor(() => container.querySelector<HTMLElement>('.contact-card')!);
		row.focus();
		await fireEvent.keyDown(row, { key: 'ContextMenu' });
		await waitFor(() => expect(container.querySelector('.overflow-menu')).not.toBeNull());
	});
});
