// @vitest-environment jsdom
// QURATOR-91 — own sent-DM history vanishes on restart. Two halves must both hold:
//
//   1. BACKEND: `send_message` must persist the sent message into the at-rest DM cache, so
//      `get_messages` returns OWN sends after a restart (pinned on the Rust side,
//      `send_message_persists_own_send_into_dm_cache` in chat.rs).
//   2. UI: wherever `inboxMessages` is seeded from the feed, entries with `from === myNpub` must be
//      merged into `sentMessages` — the conversation thread renders inbox-from-peer UNION
//      sent-to-peer, so an own-npub entry that stays only in `inboxMessages` never renders.
//
// This is the UI half's behavioural pin: mount Chat with the identity + contacts set, `getMessages`
// returning one historical peer message AND one historical OWN send, select the peer, and assert the
// own message renders in the thread. Current code reds — `sentMessages` starts empty and nothing
// seeds it from the feed.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import { get } from 'svelte/store';
import ChatPage from './+page.svelte';
import { identity, contacts, inboxMessages, sentMessages, readWatermarks } from '$lib/stores.js';

const { ME, PEER } = vi.hoisted(() => ({
	ME: 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
	PEER: 'npub1peerrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr',
}));

// $app/stores: the chat page reads `$page.url.searchParams` in an $effect; outside a real
// SvelteKit navigation context `page` is undefined there, so stub a benign store.
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/chat') }) };
});
vi.mock('$app/stores', () => stubPage);

vi.mock('$lib/api.js', () => ({
	getMessages: vi.fn().mockResolvedValue([
		{ from: PEER, to: ME, content: 'hello from the past', sent_at: '2026-08-10T10:00:00Z' },
		{ from: ME, to: PEER, content: 'my own reply, sent last week', sent_at: '2026-08-10T10:01:00Z' },
	]),
	sendMessage: vi.fn(),
	pasteKey: vi.fn().mockResolvedValue({ profile: null }),
	follow: vi.fn().mockResolvedValue(undefined),
	validateShareCode: vi.fn().mockResolvedValue(null),
	shareCodeInfo: vi.fn().mockResolvedValue(null),
	topicList: vi.fn().mockResolvedValue([]),
	topicChannel: vi.fn().mockResolvedValue({ posts: [], announcements: [] }),
	topicPost: vi.fn(),
	getContacts: vi.fn().mockResolvedValue([{ npub: PEER, collections: [], online: false }]),
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
	inboxMessages.set([]);
	sentMessages.set([]);
	readWatermarks.set({});
});

describe('QURATOR-91 — own sent history survives a restart in the thread', () => {
	it('an own-npub entry returned by getMessages renders in the selected peer thread', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([{ npub: PEER, collections: [], online: false }]);

		const { getByText } = render(ChatPage);

		// The sidebar row's preview is the NEWEST message of the thread — here the own send (10:01
		// beats the peer's 10:00). `peerPreview` marks it "You: " only for entries that reached
		// `sentMessages`, so waiting on the prefixed text is itself half the pin: without the
		// feed-seeding the preview would read the peer's plain "hello from the past".
		await waitFor(() => expect(getByText('You: my own reply, sent last week')).toBeTruthy());

		// Select the conversation (the click bubbles from the preview span to the row button). The
		// own message must render in the thread — a fresh session's `sentMessages` is empty, so the
		// own-npub entry reaches it only through the feed-seed.
		await fireEvent.click(getByText('You: my own reply, sent last week'));
		await tick();
		await waitFor(() => expect(getByText('my own reply, sent last week')).toBeTruthy());
		expect(getByText('hello from the past')).toBeTruthy();
	});

	it('a session-appended send is not dropped by the feed-seeded merge (poll semantics)', async () => {
		// The seeding must MERGE, not replace: handleSend appends to sentMessages directly, and the
		// 3s poll re-seeds from the feed afterwards. A replace-shaped seed would flash the just-sent
		// bubble out until the backend echo lands in the feed.
		const sessionSend = { from: ME, to: PEER, content: 'typed just now', sent_at: '2026-08-16T12:00:00Z' };
		sentMessages.set([sessionSend]);
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([{ npub: PEER, collections: [], online: false }]);

		const { getByText } = render(ChatPage);
		// The session send (12:00) is the newest, so it owns the row preview — proving the seeded
		// merge did NOT replace the store the test pre-populated.
		await waitFor(() => expect(getByText('You: typed just now')).toBeTruthy());
		await fireEvent.click(getByText('You: typed just now'));
		await tick();
		// Both the session send and the historical own send render together.
		await waitFor(() => expect(getByText('typed just now')).toBeTruthy());
		expect(getByText('my own reply, sent last week')).toBeTruthy();
	});
});
