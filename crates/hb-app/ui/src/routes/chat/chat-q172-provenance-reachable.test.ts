// @vitest-environment jsdom
// QURATOR-172 #1 — the Carrier-4 provenance toast was UNREACHABLE in production.
//
// `served_by` has exactly one producer: `carrier4_served_by` on the REDEEM path (fulfil.rs). The
// consuming toast lived on Browse's IMPORT path, where `open_manifest` hardcodes `served_by: None`
// — so the only code that can produce a `Some(..)` fed a value Chat threw away
// (`await redeemManifestTicket(...)`, no assignment). Two green tests sat either side of the gap
// and neither crossed it: the Rust tests prove DERIVATION, and q79-carrier4-provenance-toast
// mounts the real Browse page but injects `served_by` by mocking `importManifest` — proving
// render-given-arrival, never arrival.
//
// This test pins REACHABILITY: the value travels the production path from the redeem return to a
// rendered toast. It mounts the real Chat page and drives a real solicited ticket DM through the
// real auto-redeem (the chat-q171 pattern).
//
// WHAT IT PROVES, AND WHAT IT DOES NOT. The mock sits at the Tauri IPC boundary
// (`redeemManifestTicket`), which is the honest seam: everything below it is Rust, covered by
// fulfil.rs's own derivation tests, and the two halves meet at one command whose return type is
// shared. What was false before is precisely the half above that boundary — Chat consumed nothing.
// This is NOT the same move as q79's: there, the mock supplied a value the real backend on that
// path can never produce, so the assertion stood on an impossible input. Here the backend really
// does return `served_by` on this path.
//
// MUTATION (P-10) — in chat/+page.svelte, change
//     const note = importToast(imported, senderName);
// to
//     const note = importToast({ ...imported, served_by: undefined }, senderName);
// one line, still compiles, and models exactly the original defect (the provenance never reaches
// the toast). This test reds on the serving-peer assertion; the Browse copy tests stay green,
// which is what makes this test about reachability rather than wording.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { get } from 'svelte/store';
import { identity, contacts, inboxMessages, sentMessages, readWatermarks, toastMessage } from '$lib/stores.js';

const { ME, PEER, SERVER } = vi.hoisted(() => ({
	ME: 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
	PEER: 'npub1peerrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr',
	SERVER: 'npub1m1ram1ram1ram1ram1ram1ram1ram1ram1ram1ram1r',
}));

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
	sendCachedManifest: vi.fn(),
	redeemManifestTicket: vi.fn(),
	getSettings: vi.fn().mockResolvedValue({ big_relay_url: '' }),
	getManifestAsks: vi.fn().mockResolvedValue({}),
}));

import { redeemManifestTicket, getMessages, getManifestAsks } from '$lib/api.js';
const redeemMock = redeemManifestTicket as unknown as ReturnType<typeof vi.fn>;
const messagesMock = getMessages as unknown as ReturnType<typeof vi.fn>;
const asksMock = getManifestAsks as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	inboxMessages.set([]);
	sentMessages.set([]);
	readWatermarks.set({});
	toastMessage.set(null);
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
	[`${PEER}|criterion`]: {
		fingerprint_seen: 'fp-seen-at-ask',
		sent_at: '2026-08-01T00:00:00Z',
		nonce: 'n-abc',
	},
};

/** Mount Chat, let the solicited ticket DM auto-redeem, and hand back the live toast. */
async function redeemThroughThePage(
	imported: { slug: string; stale: boolean; served_by?: string },
): Promise<{ text: string; kind: 'success' | 'error' } | null> {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	// The SERVING peer is a contact with a petname, so a reachable provenance value must render as
	// "Mira" — the raw npub appearing instead would mean the name resolver was not applied.
	contacts.set([
		{ npub: PEER, collections: [], online: false, last_fetched: '2026-08-01T00:00:00Z', local_tags: [] },
		{ npub: SERVER, petname: 'Mira', collections: [], online: false, last_fetched: '2026-08-01T00:00:00Z', local_tags: [] },
	] as never);
	messagesMock.mockResolvedValue([
		{ from: PEER, to: ME, content: TICKET_JSON, sent_at: '2026-08-01T10:00:00Z' },
	]);
	asksMock.mockResolvedValue(ASK_TRACE);
	redeemMock.mockResolvedValue(imported);

	const { getByText } = render(ChatPage);
	// Waiting on the preview asserts the ticket DM actually arrived and the row rendered — without
	// it a broken flow would bail silently and the toast assertion would read as a plain absence.
	await waitFor(() => expect(getByText(PREVIEW_TEXT)).toBeTruthy());
	await fireEvent.click(getByText(PREVIEW_TEXT));
	await tick();
	await waitFor(() => expect(redeemMock).toHaveBeenCalled());
	await waitFor(() => expect(get(toastMessage)).not.toBeNull());
	return get(toastMessage);
}

describe('QURATOR-172 #1 — provenance from the redeem path actually reaches a toast', () => {
	it('a re-served copy names the serving peer', async () => {
		const toast = await redeemThroughThePage({ slug: 'criterion', stale: false, served_by: SERVER });
		expect(toast?.text).toContain('Mira');
		expect(toast?.text).toContain('cached copy');
		expect(toast?.text).not.toContain(SERVER); // resolved to a name, never the raw npub
		expect(toast?.kind).toBe('success');
	});

	it('a stale re-served copy says the owner is offline rather than "ask the owner"', async () => {
		// The carrier-4 case the old Browse-only copy got wrong twice: the owner being offline is
		// exactly WHY a peer re-served it, so "Ask the owner for a fresh manifest" was bad advice.
		const toast = await redeemThroughThePage({ slug: 'criterion', stale: true, served_by: SERVER });
		expect(toast?.text).toContain('Mira');
		expect(toast?.text).toContain('offline');
		expect(toast?.text).not.toContain('Ask the owner for a fresh manifest');
		expect(toast?.kind).toBe('error');
	});

	it('a direct serve carries no provenance and reads plainly', async () => {
		// `served_by: undefined` is the author-served case (`carrier4_served_by` returns None), and
		// it must NOT invent a peer. This is the arm that keeps the assertions above honest: if the
		// toast named someone here, the two above would pass for the wrong reason.
		const toast = await redeemThroughThePage({ slug: 'criterion', stale: false });
		expect(toast?.text).toBe('Full manifest imported');
		expect(toast?.text).not.toContain('Mira');
		expect(toast?.kind).toBe('success');
	});
});
