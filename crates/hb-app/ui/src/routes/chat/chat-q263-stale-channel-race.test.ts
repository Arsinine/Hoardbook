// @vitest-environment jsdom
// QURATOR-263 — loadChannel had no generation guard: select channel A then channel B quickly, and
// if A's topicChannel resolve lands after B's, A's posts/announcements bind to B's pane (and A's
// seen-watermark advances for content the user never saw). This is a BEHAVIOURAL test (real mount +
// click drive, mocking only `$lib/api.js`), same harness as q93-chat-load-error.test.ts and the same
// race-pinning shape as topics-q36-open-generation.test.ts: hold A's fetch in flight, select B, let
// B land, then release A and assert B's state survived — on the unguarded code the late resolve
// overwrites it.
//
// P-10 mutation proof (run by this lane): in crates/hb-app/ui/src/routes/chat/+page.svelte, inside
// function `loadChannel` (the QURATOR-263 region — the try block and the catch block, which sit
// between the `const generation = channelGeneration;` capture and the function's closing brace),
// neutralise the guard by making both `if (generation === channelGeneration)` conditions
// unconditional (delete the two `if` wrappers, or replace each test with `true`). Both tests below
// must go RED; restoring the guards must go GREEN. Observed here: mutation → 2 failed; restore →
// 2 passed.
//
// The watermark half of test 1 pins the announceSeen/topicAnnounceMarkSeen gate specifically: on
// the mutation, `topicAnnounceMarkSeen` is called for 'topic-A' after the stale land, which is
// exactly the silent data defect the gate exists to prevent.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts, inboxMessages, sentMessages, readWatermarks, announceSeen } from '$lib/stores.js';
import { get } from 'svelte/store';

const { ME, PEER } = vi.hoisted(() => ({
	ME: 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
	PEER: 'npub1peerrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr',
}));

// $app/stores: the page reads `$page.url.searchParams` in an $effect; stub a benign store (the
// chat-q91 pattern).
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
	topicList: vi.fn(),
	topicChannel: vi.fn(),
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

import { topicList, topicChannel, topicAnnounceMarkSeen } from '$lib/api.js';
const topicListMock = topicList as unknown as ReturnType<typeof vi.fn>;
const topicChannelMock = topicChannel as unknown as ReturnType<typeof vi.fn>;
const markSeenMock = topicAnnounceMarkSeen as unknown as ReturnType<typeof vi.fn>;

const TOPIC_A = { topic_id: 'topic-A', name: 'video/anime', description: '', tags: [], private: false, joined_at: 0 };
const TOPIC_B = { topic_id: 'topic-B', name: 'video/films', description: '', tags: [], private: false, joined_at: 0 };

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	inboxMessages.set([]);
	sentMessages.set([]);
	readWatermarks.set({});
	announceSeen.set({});
});

function primeIdentity() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
}

// Settle helper: let a just-released promise's .then chain run through Svelte's scheduler.
async function settle() {
	await tick();
	await new Promise((r) => setTimeout(r, 20));
}

describe('QURATOR-263 — loadChannel is generation-guarded against stale resolves', () => {
	it('a stale channel resolve must not bind the previous topic’s posts, nor advance its seen-watermark', async () => {
		primeIdentity();
		topicListMock.mockResolvedValue([TOPIC_A, TOPIC_B]);

		// A's channel hangs until we release it; B's resolves immediately with one post.
		let releaseA: (v: unknown) => void = () => {};
		const inFlightA = new Promise((r) => { releaseA = r; });
		topicChannelMock
			.mockReturnValueOnce(inFlightA)
			.mockResolvedValue({ posts: [{ author_npub: PEER, body: 'post from films', ts: 1_755_000_100 }], announcements: [] });

		const { getByText, queryByText } = render(ChatPage);
		await waitFor(() => expect(getByText('video/anime')).toBeTruthy());

		// Select A — its channel fetch goes out and hangs.
		await fireEvent.click(getByText('video/anime'));
		await waitFor(() => expect(topicChannelMock).toHaveBeenCalledWith('topic-A'));

		// Select B before A resolves — B's post lands and is shown.
		await fireEvent.click(getByText('video/films'));
		await waitFor(() => expect(getByText('post from films')).toBeTruthy());

		// Now let A's stale resolve land — carrying a post AND an announcement (ts > 0, so the
		// unguarded watermark advance would fire for it).
		releaseA({
			posts: [{ author_npub: PEER, body: 'post from anime', ts: 1_755_000_000 }],
			announcements: [{ author_npub: PEER, body: 'stale announce from anime', ts: 1_755_000_050 }],
		});
		await settle();

		// The watermark side-effect is gated too: A's announcements were never displayed, so marking
		// them seen would silently clear A's unread badge for content the user never read. (Asserted
		// BEFORE the posts so the full-guard mutation reds here first — under it, markSeen fires for
		// 'topic-A'.)
		expect(markSeenMock).not.toHaveBeenCalledWith('topic-A', expect.anything());
		expect(get(announceSeen)['topic-A']).toBeUndefined();

		// B's pane must still show B's post — not A's, not A's announcement.
		expect(getByText('post from films')).toBeTruthy();
		expect(queryByText('post from anime')).toBeNull();
		expect(queryByText('stale announce from anime')).toBeNull();
	});

	it('a stale channel REJECTION must not raise the channel error over the newly-selected channel', async () => {
		primeIdentity();
		topicListMock.mockResolvedValue([TOPIC_A, TOPIC_B]);

		// A's channel rejects only when we release it; B's resolves immediately.
		let rejectA: (e: unknown) => void = () => {};
		const inFlightA = new Promise((_r, rej) => { rejectA = rej; });
		topicChannelMock
			.mockReturnValueOnce(inFlightA)
			.mockResolvedValue({ posts: [{ author_npub: PEER, body: 'post from films', ts: 1_755_000_100 }], announcements: [] });

		const { getByText, queryByText, queryByRole } = render(ChatPage);
		await waitFor(() => expect(getByText('video/anime')).toBeTruthy());

		await fireEvent.click(getByText('video/anime'));
		await waitFor(() => expect(topicChannelMock).toHaveBeenCalledWith('topic-A'));

		await fireEvent.click(getByText('video/films'));
		await waitFor(() => expect(getByText('post from films')).toBeTruthy());

		// A's load fails late — the error flag must NOT land on B's pane.
		rejectA(new Error('relay unreachable'));
		await settle();

		expect(getByText('post from films')).toBeTruthy();
		expect(queryByRole('alert')).toBeNull();
		expect(queryByText(/couldn.t load this channel/i)).toBeNull();
	});
});
