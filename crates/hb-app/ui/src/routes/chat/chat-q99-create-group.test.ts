// @vitest-environment jsdom
// QURATOR-99 — Chat's group-create gives no feedback and stays open. `handleCreateGroup` called
// `groupsCreate` + `loadGroups` but neither closed `createGroupOpen` nor toasted — unlike the
// hardened siblings (contacts/+page.svelte, browse/+page.svelte, both: close + toast + refresh).
//
// Behavioural mount: drive the real chain the user drives — Requests → open a request → Accept →
// the AddContactDialog → "+ New group" → the CreateGroupDialog → type a name → Create — and assert
// the dialog CLOSES and the success toast carries the group name.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import { get } from 'svelte/store';
import ChatPage from './+page.svelte';
import { identity, contacts, dmRequests, toastMessage } from '$lib/stores.js';

const { ME, STRANGER } = vi.hoisted(() => ({
	ME: 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
	STRANGER: 'npub1strangerrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr',
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
			messages: [{ from: STRANGER, to: ME, content: 'hi', sent_at: '2026-08-14T10:00:00Z' }],
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

import { groupsCreate } from '$lib/api.js';
const groupsCreateMock = groupsCreate as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	dmRequests.set([]);
	toastMessage.set(null);
});

/** Drive: Requests → open the stranger's request → Accept → "+ New group" → CreateGroupDialog. */
async function openCreateGroupDialog(getByRole: (r: string, o?: Record<string, unknown>) => HTMLElement, getByText: (t: string) => HTMLElement) {
	await fireEvent.click(getByRole('button', { name: /message requests/i }));
	await tick();
	// Open the request bucket (its row shows the preview text).
	await fireEvent.click(getByText('hi'));
	await tick();
	// The request pane's action row: Accept opens the petname + group dialog.
	await fireEvent.click(getByRole('button', { name: /^accept$/i }));
	await tick();
	// The stacked "+ New group" link swaps in the CreateGroupDialog.
	await fireEvent.click(getByRole('button', { name: /\+ new group/i }));
	await tick();
}

describe('QURATOR-99 — chat create-group closes and toasts', () => {
	it('Create closes the dialog and toasts the new group name', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);

		const { getByRole, getByText } = render(ChatPage);

		await waitFor(() => expect(getByRole('button', { name: /message requests/i })).toBeTruthy());
		await openCreateGroupDialog(getByRole, getByText);

		// TWO dialogs are live here by design: "+ New group" opens CreateGroupDialog STACKED on the
		// still-open AddContactDialog (closing it returns there, so the new group is immediately
		// selectable). Scope every query by accessible name — a bare getByRole('dialog') finds both.
		const dialog = getByRole('dialog', { name: 'New group' });
		expect(dialog).toBeTruthy();

		// Fill the name and create.
		const nameInput = document.getElementById('cgd-name') as HTMLInputElement;
		expect(nameInput).toBeTruthy();
		await fireEvent.input(nameInput, { target: { value: 'Inner Circle' } });
		await fireEvent.click(getByRole('button', { name: /^create$/i }));
		await tick();

		// The CreateGroupDialog is gone (the AddContactDialog underneath legitimately stays open)…
		await waitFor(() => expect(() => getByRole('dialog', { name: 'New group' })).toThrow());
		// …groupsCreate fired with the name…
		expect(groupsCreateMock).toHaveBeenCalledWith('Inner Circle', expect.any(String));
		// …and the success toast names the group (current code reds: no close, no toast).
		await waitFor(() => {
			const t = get(toastMessage);
			expect(t?.text).toBe('Group "Inner Circle" created');
			expect(t?.kind).toBe('success');
		});
	});
});
