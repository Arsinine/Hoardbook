// @vitest-environment jsdom
// QURATOR-93 (Chat half) — two failed loads used to render as confident negatives:
//   1. a FAILED loadTopics silently removed the whole CHANNELS section (indistinguishable from
//      "you joined no topics");
//   2. a FAILED loadChannel (on a selected topic) showed "No posts in the last 24h" — a confident
//      negative about data the relay never returned.
//
// BEHAVIOURAL mount tests on the affordances: the CHANNELS label still renders on a failed load,
// a Retry BUTTON exists and re-runs the fetch, and the confident "No posts" string does NOT render
// while the error holds.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts, inboxMessages, sentMessages, readWatermarks } from '$lib/stores.js';

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
	// The two loads under test — individual tests flip these.
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

import { topicList, topicChannel } from '$lib/api.js';
const topicListMock = topicList as unknown as ReturnType<typeof vi.fn>;
const topicChannelMock = topicChannel as unknown as ReturnType<typeof vi.fn>;

const TOPIC = {
	topic_id: 't-video',
	name: 'video/anime',
	description: '',
	tags: ['video'],
	private: false,
	joined_at: 0,
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	inboxMessages.set([]);
	sentMessages.set([]);
	readWatermarks.set({});
});

function primeIdentity() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
}

describe('QURATOR-93 — Chat topics/channel load failures are not confident negatives', () => {
	it('topicList REJECTS → the CHANNELS section survives with an error line + Retry, and no channel rows hide as "none"', async () => {
		topicListMock.mockRejectedValue(new Error('relay unreachable'));
		primeIdentity();

		const { getByText, getByRole, queryByRole } = render(ChatPage);
		// THE core assertion: the section HEADER still renders (it used to vanish entirely).
		await waitFor(() => expect(getByText('Channels')).toBeTruthy());
		// …with the failure named and a Retry BUTTON (not the word in prose).
		expect(getByText(/couldn.t load channels/i)).toBeTruthy();
		expect(getByRole('button', { name: /retry/i })).toBeTruthy();
		// No channel row is rendered as if the list were legitimately empty.
		expect(queryByRole('button', { name: /#video\/anime/i })).toBeNull();
	});

	it('topicList Retry: reject → resolve → the channel row appears and the error line is gone', async () => {
		topicListMock.mockRejectedValueOnce(new Error('first attempt fails')).mockResolvedValue([TOPIC]);
		primeIdentity();

		const { getByText, queryByText, findByText } = render(ChatPage);
		await waitFor(() => expect(getByText(/couldn.t load channels/i)).toBeTruthy());

		await fireEvent.click(getByText('Retry'));
		await tick();

		// The channel actually lands — the both-directions rule.
		expect(await findByText('#')).toBeTruthy();
		expect(await findByText('video/anime')).toBeTruthy();
		await waitFor(() => expect(queryByText(/couldn.t load channels/i)).toBeNull());
	});

	it('topicChannel REJECTS on the selected topic → error alert + Retry, NOT "No posts in the last 24h"', async () => {
		topicListMock.mockResolvedValue([TOPIC]);
		topicChannelMock.mockRejectedValue(new Error('relay unreachable'));
		primeIdentity();

		const { getByText, findByText, queryByText, getByRole } = render(ChatPage);
		// Select the topic channel (its sidebar row).
		await findByText('video/anime');
		await fireEvent.click(getByText('video/anime'));
		await tick();

		// The failure surfaces as an alert; the confident negative must NOT render.
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());
		expect(queryByText(/no posts in the last 24h/i)).toBeNull();
	});

	it('topicChannel Retry: reject → resolve → the posts render and the alert clears', async () => {
		topicListMock.mockResolvedValue([TOPIC]);
		topicChannelMock
			.mockRejectedValueOnce(new Error('first attempt fails'))
			.mockResolvedValue({ posts: [{ author_npub: PEER, body: 'hello channel', ts: 1_755_000_000 }], announcements: [] });
		primeIdentity();

		const { getByText, findByText, queryByRole } = render(ChatPage);
		await findByText('video/anime');
		await fireEvent.click(getByText('video/anime'));
		await tick();
		await waitFor(() => expect(getByText(/couldn.t load this channel/i)).toBeTruthy());

		await fireEvent.click(getByText('Retry'));
		await tick();

		expect(await findByText('hello channel')).toBeTruthy();
		await waitFor(() => expect(queryByRole('alert')).toBeNull());
	});
});
