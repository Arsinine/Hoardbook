// @vitest-environment jsdom
// QURATOR-94 (1/2) — Block has no confirm step. A single stray click on the request-pane Block
// irreversibly dm_blocks the sender. The fix reuses the app's inline two-step confirm
// (ConfirmButton.svelte — the same pattern as Remove contact), so block-without-confirm becomes
// impossible: the resting click reveals the prompt, and only the second (Confirm) click fires
// `dm_block`.
//
// Behavioural mount: drive the real request pane and assert BOTH halves — first click reveals the
// consequence copy without firing the command; second click fires it exactly once.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts, dmRequests } from '$lib/stores.js';

const { ME, STRANGER } = vi.hoisted(() => ({
	ME: 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
	STRANGER: 'npub1strangerrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr',
}));

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
	dmRequests: vi.fn().mockResolvedValue([
		{
			npub: STRANGER,
			first_seen: 1,
			last_message_at: 2,
			message_count: 1,
			messages: [{ from: STRANGER, to: ME, content: 'yo', sent_at: '2026-08-14T10:00:00Z' }],
		},
	]),
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

import { dmBlock } from '$lib/api.js';
const dmBlockMock = dmBlock as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	dmRequests.set([]);
});

/** Drive: Requests → open the stranger's request bucket (reveals the Accept/Decline/Block row). */
async function openRequestPane(getByRole: (r: string, o?: Record<string, unknown>) => HTMLElement, getByText: (t: string) => HTMLElement) {
	await fireEvent.click(getByRole('button', { name: /message requests/i }));
	await tick();
	await fireEvent.click(getByText('yo'));
	await tick();
}

describe('QURATOR-94 — Block requires a deliberate confirm', () => {
	it('the first Block click reveals the consequence copy and does NOT fire dm_block', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);

		const { getByRole, getByText } = render(ChatPage);
		await waitFor(() => expect(getByRole('button', { name: /message requests/i })).toBeTruthy());
		await openRequestPane(getByRole, getByText);

		// First click on Block: reveals the confirm prompt, fires nothing.
		await fireEvent.click(getByRole('button', { name: /^block$/i }));
		await tick();

		expect(dmBlockMock).not.toHaveBeenCalled();
		await waitFor(() =>
			expect(getByText("Block this person? They can't message you and you won't see future requests. You can unblock in Settings.")).toBeTruthy()
		);
	});

	it('Confirm fires dm_block exactly once with the stranger npub', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);

		const { getByRole, getByText } = render(ChatPage);
		await waitFor(() => expect(getByRole('button', { name: /message requests/i })).toBeTruthy());
		await openRequestPane(getByRole, getByText);

		await fireEvent.click(getByRole('button', { name: /^block$/i }));
		await tick();
		// The revealed row's Confirm button (ConfirmButton's default confirmLabel).
		await fireEvent.click(getByRole('button', { name: /^confirm$/i }));
		await tick();

		await waitFor(() => expect(dmBlockMock).toHaveBeenCalledTimes(1));
		expect(dmBlockMock).toHaveBeenCalledWith(STRANGER);
	});

	it('Cancel withdraws the confirm without firing dm_block', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);

		const { getByRole, getByText } = render(ChatPage);
		await waitFor(() => expect(getByRole('button', { name: /message requests/i })).toBeTruthy());
		await openRequestPane(getByRole, getByText);

		await fireEvent.click(getByRole('button', { name: /^block$/i }));
		await tick();
		await fireEvent.click(getByRole('button', { name: /^cancel$/i }));
		await tick();

		expect(dmBlockMock).not.toHaveBeenCalled();
		// The resting Block trigger is back (the confirm was withdrawn).
		await waitFor(() => expect(getByRole('button', { name: /^block$/i })).toBeTruthy());
	});
});
