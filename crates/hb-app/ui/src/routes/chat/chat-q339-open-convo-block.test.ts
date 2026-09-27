// @vitest-environment jsdom
// QURATOR-339 — Block had NO affordance in an OPEN conversation. The request screen's Block
// (QURATOR-94) disappears the moment a request is accepted, and the pane header offered only
// "View profile" — so blocking an already-open chat partner meant leaving Chat to paste their
// npub into Settings. The fix puts the same two-step ConfirmButton in the pane header and, when
// the peer is a contact, offers — as a choice, never a silent bundle (owner design direction,
// 2026-09-26) — to also remove them from Contacts. Blocking gates chat/DM only: no copy may
// promise any other revocation (CLAUDE.md §6).
//
// Behavioural mount (P-4): drive the REAL page via the `?peer=` deep-link — a contact resolves
// through the settled `$contacts` store, a stranger through the pasteKey path QURATOR-146 built —
// and assert all three acceptance halves against the real components.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts } from '$lib/stores.js';
import { page as pageStore } from '$app/stores';
import type { Writable } from 'svelte/store';
import type { ContactSummary } from '$lib/types.js';
// The mock below makes it writable; the SvelteKit type says Readable.
const page = pageStore as unknown as Writable<{ url: URL }>;

const { ME, CONTACT, STRANGER } = vi.hoisted(() => ({
	ME: 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
	CONTACT: 'npub1contacttttttttttttttttttttttttttttttttttttttttttttttttttt',
	STRANGER: 'npub1strangerrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr',
}));

// $app/stores: outside a real SvelteKit navigation context `page` is undefined in the page's
// $effect, so stub a WRITABLE store — each test aims the `?peer=` deep-link at its own npub.
const stubPage = vi.hoisted(async () => {
	const { writable } = await import('svelte/store');
	return { page: writable({ url: new URL('http://localhost/chat') }) };
});
vi.mock('$app/stores', () => stubPage);

vi.mock('$lib/api.js', () => ({
	getMessages: vi.fn().mockResolvedValue([]),
	sendMessage: vi.fn(),
	pasteKey: vi.fn().mockResolvedValue({ npub: STRANGER, profile: null }),
	follow: vi.fn().mockResolvedValue(undefined),
	validateShareCode: vi.fn().mockResolvedValue(null),
	shareCodeInfo: vi.fn().mockResolvedValue(null),
	topicList: vi.fn().mockResolvedValue([]),
	topicChannel: vi.fn().mockResolvedValue({ posts: [], announcements: [] }),
	topicPost: vi.fn(),
	getContacts: vi.fn().mockResolvedValue([]),
	unfollowContact: vi.fn().mockResolvedValue(undefined),
	dmRequests: vi.fn().mockResolvedValue([]),
	dmRequestAccept: vi.fn().mockResolvedValue([]),
	dmRequestDecline: vi.fn(),
	dmBlock: vi.fn().mockResolvedValue(undefined),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn().mockResolvedValue(undefined),
	contactUpdateGroups: vi.fn(),
	advanceReadWatermark: vi.fn().mockResolvedValue(undefined),
	topicAnnounceMarkSeen: vi.fn(),
	getShareCode: vi.fn().mockResolvedValue(''),
	relayStatus: vi.fn().mockResolvedValue([]),
	getCollections: vi.fn().mockResolvedValue([]),
	exportManifest: vi.fn(),
	sendFullList: vi.fn(),
	redeemManifestTicket: vi.fn(),
	getSettings: vi.fn().mockResolvedValue({ big_relay_url: '' }),
	getManifestAsks: vi.fn().mockResolvedValue([]),
}));

import { dmBlock, unfollowContact } from '$lib/api.js';
const dmBlockMock = dmBlock as unknown as ReturnType<typeof vi.fn>;
const unfollowContactMock = unfollowContact as unknown as ReturnType<typeof vi.fn>;

/** A minimal ContactSummary — only `npub` matters to the chat roster lookup. */
function contactFixture(npub: string): ContactSummary {
	return { npub, has_browse_key: false, collections: [], online: false, last_fetched: null, local_tags: [] } as unknown as ContactSummary;
}

const BLOCK_CONFIRM_TEXT = "Block this person? They can't message you and you won't see future requests. You can unblock in Settings.";
const DELIST_ASK = 'Blocked. Also remove this person from Contacts?';

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	page.set({ url: new URL('http://localhost/chat') });
});

/** Drive: aim the `?peer=` deep-link at `npub` and mount — the contact resolves through settled
 *  `$contacts`, a stranger through pasteKey — then wait for the conversation pane header. */
async function openConversation(npub: string) {
	page.set({ url: new URL('http://localhost/chat?peer=' + npub) });
	const ui = render(ChatPage);
	await waitFor(() => expect(ui.getByRole('button', { name: /view profile/i })).toBeTruthy());
	return ui;
}

/** Drive: click the header Block, then its revealed Confirm, and wait for dm_block to land. */
async function blockViaConfirm(getByRole: (r: string, o?: Record<string, unknown>) => HTMLElement) {
	await fireEvent.click(getByRole('button', { name: /^block$/i }));
	await tick();
	await fireEvent.click(getByRole('button', { name: /^confirm$/i }));
	await waitFor(() => expect(dmBlockMock).toHaveBeenCalledTimes(1));
}

describe('QURATOR-339 — Block in an open conversation', () => {
	// P-10 mutation: neutralising the header wiring — `onconfirm={() => handleBlockPeer()}` →
	// `onconfirm={() => {}}` in the pane-header ConfirmButton — must red this test (dm_block never
	// fires). Deleting the header ConfirmButton outright reds it earlier, at the Block getByRole.
	it('header Block: first click reveals the consequence copy and fires nothing; Confirm fires dm_block exactly once for this npub', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([contactFixture(CONTACT)]);

		const { getByRole, getByText } = await openConversation(CONTACT);

		// First click: reveal only.
		await fireEvent.click(getByRole('button', { name: /^block$/i }));
		await tick();
		expect(dmBlockMock).not.toHaveBeenCalled();
		expect(getByText(BLOCK_CONFIRM_TEXT)).toBeTruthy();

		// Confirm: exactly once, for THIS peer.
		await fireEvent.click(getByRole('button', { name: /^confirm$/i }));
		await waitFor(() => expect(dmBlockMock).toHaveBeenCalledTimes(1));
		expect(dmBlockMock).toHaveBeenCalledWith(CONTACT);
	});

	// P-10 mutation: `if (selectedIsContact) blockDelistPrompt = true;` → `if (false)
	// blockDelistPrompt = true;` must red this test (the ask never appears). Deleting the
	// `await unfollowContact(npub);` line in handleBlockDelist reds it at the call-count assertion.
	it('blocking a peer who IS a contact offers the delist ask; choosing Remove calls unfollowContact exactly once', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([contactFixture(CONTACT)]);

		const { getByRole, getByText } = await openConversation(CONTACT);
		await blockViaConfirm(getByRole);

		// The ask appears after the block — and nothing fires until it is answered.
		await waitFor(() => expect(getByText(DELIST_ASK)).toBeTruthy());
		expect(unfollowContactMock).not.toHaveBeenCalled();

		await fireEvent.click(getByRole('button', { name: /remove from contacts/i }));
		await waitFor(() => expect(unfollowContactMock).toHaveBeenCalledTimes(1));
		expect(unfollowContactMock).toHaveBeenCalledWith(CONTACT);
	});

	// P-10 mutation: adding `unfollowContact(selectedPeer!.npub);` inside handleBlockKeep (the
	// silent-bundle regression the owner ruled out) must red this test.
	it('choosing Keep contact withdraws the ask and does NOT call unfollowContact', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([contactFixture(CONTACT)]);

		const { getByRole, getByText, queryByText } = await openConversation(CONTACT);
		await blockViaConfirm(getByRole);
		await waitFor(() => expect(getByText(DELIST_ASK)).toBeTruthy());

		await fireEvent.click(getByRole('button', { name: /keep contact/i }));
		await tick();
		expect(unfollowContactMock).not.toHaveBeenCalled();
		expect(queryByText(DELIST_ASK)).toBeNull();
	});

	// P-10 mutation: dropping the contact guard — `if (selectedIsContact) blockDelistPrompt = true;`
	// → `blockDelistPrompt = true;` — must red this test (a NON-contact gets the ask).
	it('blocking a deep-linked NON-contact fires dm_block with no delist ask; unfollowContact never called', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);

		const { getByRole, queryByText } = await openConversation(STRANGER);
		await blockViaConfirm(getByRole);

		await tick();
		await tick();
		expect(queryByText(DELIST_ASK)).toBeNull();
		expect(unfollowContactMock).not.toHaveBeenCalled();
	});

	// P-10 mutation: rewriting the header confirmText to promise a revocation — e.g. "…This revokes
	// their access…" — must red this test on both prongs.
	it('no Block copy promises an access revocation: the confirm text and the delist ask name only message gating and the contact entry (§6)', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([contactFixture(CONTACT)]);

		const { getByRole, getByText } = await openConversation(CONTACT);

		// Prong 1: the revealed confirm text — exactly the pinned string, no revocation language.
		await fireEvent.click(getByRole('button', { name: /^block$/i }));
		await tick();
		const confirmEl = getByText(BLOCK_CONFIRM_TEXT);
		expect(confirmEl.textContent).toMatch(/can't message you/); // names what IS gated (chat/DM)
		expect(confirmEl.textContent?.toLowerCase()).not.toMatch(/access|revoke|withdraw/);

		// Prong 2: the post-block delist ask — names the CONTACT entry only.
		await fireEvent.click(getByRole('button', { name: /^confirm$/i }));
		await waitFor(() => expect(getByText(DELIST_ASK)).toBeTruthy());
		const askEl = getByText(DELIST_ASK);
		expect(askEl.textContent).toMatch(/Contacts/);
		expect(askEl.textContent?.toLowerCase()).not.toMatch(/access|revoke|withdraw/);
		expect(unfollowContactMock).not.toHaveBeenCalled();
	});
});
