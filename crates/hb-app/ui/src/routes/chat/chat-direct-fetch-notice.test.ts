// @vitest-environment jsdom
// The one-time direct-connection notice. Redeeming a ticket DIALS the sender, so both sides see
// each other's IP; the pre-first-download notice that used to say so lost its only caller with
// file transfer in v0.9.6 (0905b9df). This pins its return: shown once, BEFORE the dial, never
// again once acknowledged — and never a gate (OK-only, the fetch proceeds after it).
//
// Mount pattern and fixtures follow chat-q171-redeem-fingerprint.test.ts.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts, inboxMessages, sentMessages, readWatermarks } from '$lib/stores.js';
import { DIRECT_FETCH_NOTICE } from '$lib/privacy-gate.js';

const { ME, PEER, order } = vi.hoisted(() => ({
	ME: 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
	PEER: 'npub1peerrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr',
	order: [] as string[],
}));

const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/chat') }) };
});
vi.mock('$app/stores', () => stubPage);

vi.mock('@tauri-apps/plugin-dialog', () => ({
	message: vi.fn(async () => { order.push('notice'); }),
	confirm: vi.fn().mockResolvedValue(true),
	ask: vi.fn().mockResolvedValue(true),
	open: vi.fn(),
	save: vi.fn(),
}));

vi.mock('$lib/api.js', () => ({
	getMessages: vi.fn(),
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
	sendCachedManifest: vi.fn(),
	redeemManifestTicket: vi.fn(async () => { order.push('dial'); return { slug: 'criterion', stale: false }; }),
	getSettings: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	acknowledgePrivacyNotice: vi.fn().mockResolvedValue(undefined),
}));

import { message } from '@tauri-apps/plugin-dialog';
import { redeemManifestTicket, getManifestAsks, getSettings, getMessages, acknowledgePrivacyNotice } from '$lib/api.js';
const messageMock = message as unknown as ReturnType<typeof vi.fn>;
const redeemMock = redeemManifestTicket as unknown as ReturnType<typeof vi.fn>;
const settingsMock = getSettings as unknown as ReturnType<typeof vi.fn>;
const ackMock = acknowledgePrivacyNotice as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	order.length = 0;
	identity.set(null);
	contacts.set([]);
	inboxMessages.set([]);
	sentMessages.set([]);
	readWatermarks.set({});
});

const TICKET_JSON = JSON.stringify({
	hb: 'transport_ticket',
	request_id: 'req-1',
	slug: 'criterion',
	issued_at: 1756_000_000,
	ask_nonce: 'n-abc',
});
const PREVIEW_TEXT = TICKET_JSON.replace(/\s+/g, ' ').trim().slice(0, 48) + '…';
const ASK_TRACE = {
	[`${PEER}|criterion`]: { fingerprint_seen: 'fp', sent_at: '2026-08-01T00:00:00Z', nonce: 'n-abc' },
};

async function openSolicitedTicket(acknowledged: boolean) {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	contacts.set([{ npub: PEER, has_browse_key: false, collections: [], online: false, last_fetched: '2026-08-01T00:00:00Z', local_tags: [] }]);
	(getMessages as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
		{ from: PEER, to: ME, content: TICKET_JSON, sent_at: '2026-08-01T10:00:00Z' },
	]);
	(getManifestAsks as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(ASK_TRACE);
	settingsMock.mockResolvedValue({ big_relay_url: '', privacy_notice_acknowledged: acknowledged });
	const { getByText } = render(ChatPage);
	await waitFor(() => expect(getByText(PREVIEW_TEXT)).toBeTruthy());
	await fireEvent.click(getByText(PREVIEW_TEXT));
	await tick();
	await waitFor(() => expect(redeemMock).toHaveBeenCalled());
}

describe('direct-fetch notice', () => {
	it('first direct fetch: the notice shows BEFORE the dial, then is recorded as acknowledged', async () => {
		// MUTATION (P-10): in routes/chat/+page.svelte, delete the line
		// `await showDirectFetchNoticeOnce();` inside redeem() — this test reds (no notice).
		await openSolicitedTicket(false);
		expect(messageMock).toHaveBeenCalledTimes(1);
		expect(messageMock.mock.calls[0][0]).toBe(DIRECT_FETCH_NOTICE);
		expect(order).toEqual(['notice', 'dial']);
		expect(ackMock).toHaveBeenCalledTimes(1);
	});

	it('already acknowledged: no notice, and the fetch still goes ahead', async () => {
		// MUTATION (P-10): in lib/privacy-gate.ts, delete
		// `if (!shouldShowPrivacyNotice(s)) return;` — this test reds (notice shows again).
		await openSolicitedTicket(true);
		expect(messageMock).not.toHaveBeenCalled();
		expect(order).toEqual(['dial']);
	});

	it('the copy matches what Hoardbook does today: no file downloads, a full list over a direct connection', () => {
		expect(DIRECT_FETCH_NOTICE).not.toMatch(/download/i);
		expect(DIRECT_FETCH_NOTICE).toMatch(/full list/);
		expect(DIRECT_FETCH_NOTICE).toMatch(/see your IP/);
	});
});
