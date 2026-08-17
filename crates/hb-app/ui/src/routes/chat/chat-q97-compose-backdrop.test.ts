// @vitest-environment jsdom
// QURATOR-97 (chat's share) — the compose-to-npub modal holds typed state (recipient + message
// body); a stray backdrop click silently discards it. The modal must not close on backdrop
// (closeOnBackdrop={false}); Cancel and Escape stay deliberate actions.
//
// Same behavioural shape as topics-q97-backdrop.test.ts: open the real modal, click the real
// backdrop, assert the dialog survives AND the typed value survived.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts } from '$lib/stores.js';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';

// $app/stores: the chat page reads `$page.url.searchParams` in an $effect; outside a real
// SvelteKit navigation context `page` is undefined there, so stub a benign store.
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/chat') }) };
});
vi.mock('$app/stores', () => stubPage);

vi.mock('$lib/api.js', () => ({
	getMessages: vi.fn().mockResolvedValue([]),
	sendMessage: vi.fn(),
	pasteKey: vi.fn().mockResolvedValue({ profile: null }),
	follow: vi.fn().mockResolvedValue(undefined),
	validateShareCode: vi.fn().mockResolvedValue(null),
	shareCodeInfo: vi.fn().mockResolvedValue(null),
	topicList: vi.fn().mockResolvedValue([]),
	topicChannel: vi.fn().mockResolvedValue({ posts: [], announcements: [] }),
	topicPost: vi.fn(),
	getContacts: vi.fn().mockResolvedValue([]),
	dmRequests: vi.fn().mockResolvedValue([]),
	dmRequestAccept: vi.fn(),
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

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
});

describe('QURATOR-97 — chat compose modal ignores backdrop clicks', () => {
	it('a backdrop click neither closes the modal nor wipes the typed body', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);

		const { getByRole, getByPlaceholderText } = render(ChatPage);

		// Open the compose modal (the + icon-btn beside refresh).
		await fireEvent.click(getByRole('button', { name: /new message/i }));
		await tick();
		expect(getByRole('dialog')).toBeTruthy();

		// Type something worth protecting.
		const body = getByPlaceholderText(/message…/i) as HTMLTextAreaElement;
		await fireEvent.input(body, { target: { value: 'half-written first contact' } });

		// Click the backdrop itself (target === currentTarget), not the card.
		const backdrop = document.querySelector('.modal-backdrop') as HTMLElement;
		expect(backdrop).toBeTruthy();
		await fireEvent.click(backdrop);
		await tick();

		// The modal is still open and the typed value survived.
		expect(getByRole('dialog')).toBeTruthy();
		expect((getByPlaceholderText(/message…/i) as HTMLTextAreaElement).value).toBe('half-written first contact');
	});

	it('Cancel stays a deliberate close (the affordance the ticket keeps)', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);

		const { getByRole } = render(ChatPage);
		await fireEvent.click(getByRole('button', { name: /new message/i }));
		await tick();
		await fireEvent.click(getByRole('button', { name: /cancel/i }));
		await waitFor(() => expect(() => getByRole('dialog')).toThrow());
	});
});
