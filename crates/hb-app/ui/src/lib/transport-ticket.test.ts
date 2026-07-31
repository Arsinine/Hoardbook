import { describe, expect, it } from 'vitest';
import {
	parseTransportTicket,
	transportTicketHint,
	RedemptionLedger,
	wasAsked,
	TICKET_TAG,
	REDEEM_FAILED_LINE,
	SEND_FULL_LIST_LABEL,
} from './transport-ticket.js';
import { parseManifestRequest } from './request-inbox.js';

const ticketBody = (over: Record<string, unknown> = {}) =>
	JSON.stringify({
		hb: TICKET_TAG,
		ticket_v: 1,
		request_id: 'a1b2c3',
		slug: 'criterion',
		node_addr: '{"id":"…"}',
		issued_at: 1_700_000_000,
		...over,
	});

describe('parseTransportTicket', () => {
	it('parses a ticket DM into its bindings', () => {
		expect(parseTransportTicket(ticketBody())).toEqual({
			requestId: 'a1b2c3',
			slug: 'criterion',
			issuedAt: 1_700_000_000,
		});
	});

	it('returns null for an ordinary chat message and for non-JSON', () => {
		expect(parseTransportTicket('hey, did you get it?')).toBeNull();
		expect(parseTransportTicket('{not json')).toBeNull();
		expect(parseTransportTicket('null')).toBeNull();
	});

	/** The two structured bodies travel in OPPOSITE directions — a request is asker→owner and asks for
	 *  something; a ticket is owner→asker and grants it. Confusing them would render a fulfil card on
	 *  a grant (offering to send yourself your own list) or a redemption on an ask (dialling the person
	 *  who asked). Each parser must reject the other's body outright. */
	it('a manifest_request is not a ticket, and a ticket is not a manifest_request', () => {
		const request = JSON.stringify({
			hb: 'manifest_request',
			slug: 'criterion',
			fingerprint_seen: 'fp1',
		});
		expect(parseTransportTicket(request)).toBeNull();
		expect(parseManifestRequest(ticketBody())).toBeNull();
	});

	/** ⚠ The test above passes **for the wrong reason** on its own, and this one exists because trying
	 *  to break it proved that. Deleting the `hb` check from `parseTransportTicket` left it green: a
	 *  `manifest_request` body has no `request_id`, so the binding check rejected it anyway, and the
	 *  discriminator was never actually exercised.
	 *
	 *  This body is byte-identical to a valid ticket **except** for the tag, so it can only be refused
	 *  by the discriminator. Same class as the framing test that used a `Cursor` and hit EOF — right
	 *  outcome, wrong reason, green either way. */
	it('refuses a body that is a valid ticket in every respect BUT the discriminator', () => {
		expect(parseTransportTicket(ticketBody({ hb: 'manifest_request' }))).toBeNull();
		expect(parseTransportTicket(ticketBody({ hb: 'something_else' }))).toBeNull();
		expect(parseTransportTicket(ticketBody({ hb: undefined }))).toBeNull();
	});

	/** Recognition must be strict about the bindings, not just the tag. A body missing `request_id`
	 *  or `slug` is not an under-detailed ticket — it is something else, and rendering a card for it
	 *  would start a redemption that can only fail. */
	it('refuses a ticket missing either binding', () => {
		expect(parseTransportTicket(ticketBody({ request_id: undefined }))).toBeNull();
		expect(parseTransportTicket(ticketBody({ request_id: '' }))).toBeNull();
		expect(parseTransportTicket(ticketBody({ slug: undefined }))).toBeNull();
		expect(parseTransportTicket(ticketBody({ slug: '' }))).toBeNull();
	});

	it('renders a human hint instead of raw JSON', () => {
		expect(transportTicketHint(ticketBody())).toBe('Sent you the full list of “criterion”');
		expect(transportTicketHint('an ordinary message')).toBeNull();
	});
});

describe('RedemptionLedger', () => {
	/** The whole reason the ledger exists: the same ticket DM is re-rendered on every 3s poll and
	 *  re-read from the encrypted cache across restarts. Without a once-only claim, a manifest the
	 *  user successfully received would dial the owner in a loop and show a stream of
	 *  "already redeemed" errors for a success. */
	it('claims a request id exactly once, however many times it is rendered', () => {
		const l = new RedemptionLedger();
		expect(l.claim('req-1')).toBe(true);
		expect(l.claim('req-1')).toBe(false);
		expect(l.claim('req-1')).toBe(false);
		expect(l.get('req-1')).toEqual({ kind: 'redeeming' });
	});

	it('keys per request, so two tickets redeem independently', () => {
		const l = new RedemptionLedger();
		expect(l.claim('req-1')).toBe(true);
		expect(l.claim('req-2')).toBe(true);
	});

	it('a settled redemption is never re-claimed by a render', () => {
		const l = new RedemptionLedger();
		l.claim('req-1');
		l.succeed('req-1');
		expect(l.get('req-1')).toEqual({ kind: 'done' });
		expect(l.claim('req-1')).toBe(false);
	});

	/** Retry is bounded to the failed state ON PURPOSE. That boundary is what keeps it from becoming a
	 *  general "redeem whenever you like" affordance — redemption is immediate by owner ruling
	 *  (2026-07-30), and the backend has no deferred entry point at all. A retry after a failure is the
	 *  same immediate redemption re-attempted, which is safe because a failed attempt does not spend
	 *  the ticket. */
	it('permits a retry only from a failure — never from in-flight or success', () => {
		const l = new RedemptionLedger();
		l.claim('req-1');
		expect(l.claimRetry('req-1')).toBe(false); // in flight
		l.succeed('req-1');
		expect(l.claimRetry('req-1')).toBe(false); // already delivered
		l.fail('req-1', 'dial failed');
		expect(l.claimRetry('req-1')).toBe(true);
		expect(l.get('req-1')).toEqual({ kind: 'redeeming' });
	});

	it('never retries a ticket it has not seen', () => {
		expect(new RedemptionLedger().claimRetry('never-seen')).toBe(false);
	});

	it('surfaces the failure message for the card', () => {
		const l = new RedemptionLedger();
		l.claim('req-1');
		l.fail('req-1', 'dial the manifest plane: timed out');
		expect(l.get('req-1')).toEqual({
			kind: 'failed',
			message: 'dial the manifest plane: timed out',
		});
	});
});

describe('wasAsked — the unsolicited-ticket gate', () => {
	/** **This is an IP-exposure control, not tidiness.** Redemption dials the owner, and the card
	 *  fires on render — so without this gate any contact could drop a ticket for a collection we
	 *  never asked about into our inbox and make us connect to them on sight, handing over our
	 *  address. That is the H4/MT2 harvest arriving through a different door than the one presence was
	 *  hardened against. */
	it('is true only for a peer+collection we actually asked', () => {
		const asks = { 'npub1owner|criterion': { sent_at: 'x' } };
		expect(wasAsked(asks, 'npub1owner', 'criterion')).toBe(true);
		// Right peer, collection we never asked about.
		expect(wasAsked(asks, 'npub1owner', 'other')).toBe(false);
		// Right collection, a peer we never asked — the impersonation case.
		expect(wasAsked(asks, 'npub1stranger', 'criterion')).toBe(false);
	});

	/** Fails CLOSED while the trace is loading. A `null` that read as "asked" would make every launch
	 *  a window in which an unsolicited ticket auto-dials. */
	it('is false when the ask trace has not loaded yet', () => {
		expect(wasAsked(null, 'npub1owner', 'criterion')).toBe(false);
	});

	it('is false against an empty trace', () => {
		expect(wasAsked({}, 'npub1owner', 'criterion')).toBe(false);
	});

	/** The lookup must not be satisfied by inherited Object properties — `asks['constructor']` is
	 *  truthy on a plain object, so a slug of "constructor" would otherwise read as asked. */
	it('is not fooled by inherited Object properties', () => {
		expect(wasAsked({}, 'npub1owner', 'constructor')).toBe(false);
		expect(wasAsked({}, 'toString', 'constructor')).toBe(false);
	});
});

describe('copy', () => {
	/** The verb is the whole point. What crosses the plane is the LIST; the files stay with their
	 *  owner (INV-4′, MAS-INV-5). Copy that said "download" or "send the files" would describe an
	 *  affordance that does not exist and that the invariant forbids. */
	it('the fulfil verb names the list, never the files or a download', () => {
		expect(SEND_FULL_LIST_LABEL).toBe('Send the full list');
		expect(SEND_FULL_LIST_LABEL).not.toMatch(/download|file/i);
	});

	/** A failed redemption must say the request survives, because it does — the owner spends the
	 *  ticket only on this side's acknowledgement. Copy implying the user has to ask again would send
	 *  them to re-request something they can simply retry. */
	it('the failure line tells the user their request still stands', () => {
		expect(REDEEM_FAILED_LINE).toMatch(/still good/i);
	});
});
