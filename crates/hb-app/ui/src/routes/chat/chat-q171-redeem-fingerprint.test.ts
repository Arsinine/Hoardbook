// @vitest-environment jsdom
// QURATOR-171 — Defect A: `redeem()` called `redeemManifestTicket` with two arguments, so the
// third (the asker's newest-known fingerprint) was always undefined → null on the wire, and the
// backend's staleness gate (`stale = newest_fingerprint.map(|fp| !envelope.matches_fingerprint(fp))`)
// was permanently false — precisely for re-served cached copies, which can be arbitrarily old.
//
// MOUNT test (the route CAN mount — chat-q91/q93/q155 are the proof): a solicited ticket DM
// auto-redeems ON ARRIVAL, and the redeem call must FORWARD the ask record's `fingerprint_seen`
// as the third argument. The ask record is the honest source: the nonce that authorizes the dial
// and the fingerprint were written in ONE store insert (record_manifest_ask), so the record that
// authorizes redemption is exactly the record that carries the fingerprint.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The mutation
// probe (drop the askFingerprintSeen(...) argument from the redeem(...) call inside
// redemptionFor, re-run this file) MUST fail the mount test. The helper tests red on removing the
// fingerprint read-back itself.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts, inboxMessages, sentMessages, readWatermarks } from '$lib/stores.js';
import { askFingerprintSeen } from '$lib/transport-ticket.js';

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
	redeemManifestTicket: vi.fn().mockResolvedValue({ slug: 'criterion', stale: false }),
	getSettings: vi.fn().mockResolvedValue({ big_relay_url: '' }),
	getManifestAsks: vi.fn().mockResolvedValue([]),
}));

import { redeemManifestTicket, getManifestAsks } from '$lib/api.js';
const redeemMock = redeemManifestTicket as unknown as ReturnType<typeof vi.fn>;
const asksMock = getManifestAsks as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
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
// The sidebar preview truncates to 48 chars and appends '…' when longer (chat-preview.truncate).
const PREVIEW_TEXT = TICKET_JSON.replace(/\s+/g, ' ').trim().slice(0, 48) + '…';
const ASK_TRACE = {
	// Owner-path key `npub|slug` — the record whose nonce authorized the dial.
	[`${PEER}|criterion`]: {
		fingerprint_seen: 'fp-seen-at-ask',
		sent_at: '2026-08-01T00:00:00Z',
		nonce: 'n-abc',
	},
};

describe("QURATOR-171 — redeem forwards the asker's newest-known fingerprint", () => {
	it("mount: a solicited ticket DM auto-redeems with the ask record's fingerprint_seen as the 3rd arg", async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([{ npub: PEER, collections: [], online: false, last_fetched: '2026-08-01T00:00:00Z', local_tags: [] }]);

		const { getMessages } = await import('$lib/api.js');
		(getMessages as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
			{ from: PEER, to: ME, content: TICKET_JSON, sent_at: '2026-08-01T10:00:00Z' },
		]);
		asksMock.mockResolvedValue(ASK_TRACE);

		const { getByText } = render(ChatPage);

		// Select the conversation (the click bubbles from the preview span to the row button — the
		// chat-q91 pattern). Waiting on the preview text itself asserts the ticket DM arrived and the
		// peer row rendered — no silent bail if the flow breaks earlier.
		await waitFor(() => expect(getByText(PREVIEW_TEXT)).toBeTruthy());
		await fireEvent.click(getByText(PREVIEW_TEXT));
		await tick();

		// THE pin: redemption fired, and the third argument is the ask record's fingerprint_seen —
		// not undefined (today's defect), not null-on-the-wire.
		await waitFor(() => expect(redeemMock).toHaveBeenCalled());
		expect(redeemMock).toHaveBeenCalledTimes(1);
		expect(redeemMock).toHaveBeenCalledWith(PEER, TICKET_JSON, 'fp-seen-at-ask');
	});

	it('helper: askFingerprintSeen returns the record whose nonce the ticket echoes', () => {
		// Owner path, authorless ticket (author === responder), legacy 2-segment key: the echoed
		// nonce identifies which record is THE one — a record for the same (peer, slug) carrying a
		// DIFFERENT nonce is a different (older) ask and must not be read.
		expect(askFingerprintSeen(ASK_TRACE, PEER, 'criterion', 'n-abc', undefined)).toBe('fp-seen-at-ask');
		const bothKeys = {
			[`${PEER}|criterion`]: { nonce: 'n-old', fingerprint_seen: 'fp-old' },
			[`${PEER}|${PEER}|criterion`]: { nonce: 'n-new', fingerprint_seen: 'fp-new' },
		};
		// The widened self-author spelling of the same ask: the ticket carrying the CURRENT nonce
		// resolves there — never to the older legacy record sitting under the legacy key.
		expect(askFingerprintSeen(bothKeys, PEER, 'criterion', 'n-new', undefined)).toBe('fp-new');
		// A re-serve ticket naming a third-party author must not read the authorless record —
		// that fallback is the cross-tenant collision.
		const onlyLegacy = { [`${PEER}|criterion`]: { nonce: 'n-abc', fingerprint_seen: 'fp-legacy' } };
		expect(askFingerprintSeen(onlyLegacy, PEER, 'criterion', 'n-abc', 'npub1authorrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr')).toBeUndefined();
		// Trace not loaded (or its read failed) ⇒ nothing to gate on — never a guess.
		expect(askFingerprintSeen(null, PEER, 'criterion', 'n-abc', undefined)).toBeUndefined();
		// No nonce on the ticket ⇒ undefined (a nonceless ticket can never have authorized a dial).
		expect(askFingerprintSeen(ASK_TRACE, PEER, 'criterion', undefined, undefined)).toBeUndefined();
	});

	it('helper: no fingerprint recorded or unknown collection ⇒ undefined (nothing to gate on)', () => {
		const noFp = { [`${PEER}|criterion`]: { nonce: 'n-abc', fingerprint_seen: '' } };
		expect(askFingerprintSeen(noFp, PEER, 'criterion', 'n-abc', undefined)).toBeUndefined();
		expect(askFingerprintSeen(ASK_TRACE, PEER, 'other-slug', 'n-abc', undefined)).toBeUndefined();
	});
});
