import { describe, expect, it } from 'vitest';
import {
	parseTransportTicket,
	transportTicketHint,
	RedemptionLedger,
	ticketAnswersOurAsk,
	askIdentity,
	TICKET_TAG,
	REDEEM_FAILED_LINE,
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
	const ASK = askIdentity('npub1owner', 'criterion', 'n-abc');

	/** Render-idempotence: the same DM re-renders on every 3 s poll and is re-read from cache across
	 *  restarts. Without a once-only claim a *successful* redemption would dial in a loop and show
	 *  "already redeemed" errors for a success. */
	it('claims a request id exactly once, however many times it is rendered', () => {
		const l = new RedemptionLedger();
		expect(l.claim('req-1', ASK)).toBe(true);
		expect(l.claim('req-1', ASK)).toBe(false);
		expect(l.get('req-1')).toEqual({ kind: 'redeeming' });
	});

	/** **The exploit this scoping exists to stop.** A peer that has seen one valid nonce can mint N
	 *  tickets, each with a different OWNER-chosen `request_id` and a different node address, all
	 *  echoing that one nonce. Claiming per request id let every one of them dial before any
	 *  completed — one ask became arbitrary concurrent dials to endpoints of the peer's choosing. */
	it('admits ONE in-flight redemption per ask, however many request ids a peer invents', () => {
		const l = new RedemptionLedger();
		expect(l.claim('peer-chosen-1', ASK)).toBe(true);
		expect(l.claim('peer-chosen-2', ASK)).toBe(false);
		expect(l.claim('peer-chosen-3', ASK)).toBe(false);
	});

	it('keys per ask, so two genuine asks redeem independently', () => {
		const l = new RedemptionLedger();
		const other = askIdentity('npub1owner', 'other', 'n-xyz');
		expect(l.claim('req-1', ASK)).toBe(true);
		expect(l.claim('req-2', other)).toBe(true);
	});

	/** Once an ask is answered it is spent for the session — including for a *different* ticket
	 *  echoing the same nonce, which is how a peer would otherwise get a second delivery. */
	it('spends the ask on success, refusing any later ticket for it', () => {
		const l = new RedemptionLedger();
		l.claim('req-1', ASK);
		l.succeed('req-1', ASK);
		expect(l.isSpent(ASK)).toBe(true);
		expect(l.claim('req-2', ASK)).toBe(false);
	});

	/** **Independent of the durable consume on purpose.** If the backend's delete fails (disk full,
	 *  permissions) the reusable authorization would otherwise come straight back; this holds it
	 *  closed until a restart, and a restart needs a fresh ask to re-authorize. */
	it('the spent marker does not depend on the backend delete succeeding', () => {
		const l = new RedemptionLedger();
		l.claim('req-1', ASK);
		l.succeed('req-1', ASK); // no backend involved here at all
		expect(l.claim('req-9', ASK)).toBe(false);
	});

	/** A failure RELEASES the ask rather than spending it — a dial that never connected cost nothing
	 *  and must stay retryable, exactly as the ticket itself does. */
	it('releases the ask on failure so a retry is possible', () => {
		const l = new RedemptionLedger();
		l.claim('req-1', ASK);
		l.fail('req-1', ASK, 'dial failed');
		expect(l.isSpent(ASK)).toBe(false);
		expect(l.claimRetry('req-1', ASK)).toBe(true);
	});

	/** Retry stays bounded to the failed state, and cannot become a second concurrent dial. */
	it('permits a retry only from a failure, and never while the ask is in flight or spent', () => {
		const l = new RedemptionLedger();
		l.claim('req-1', ASK);
		expect(l.claimRetry('req-1', ASK)).toBe(false); // in flight
		l.succeed('req-1', ASK);
		expect(l.claimRetry('req-1', ASK)).toBe(false); // spent
	});

	it('never retries a ticket it has not seen', () => {
		expect(new RedemptionLedger().claimRetry('never-seen', ASK)).toBe(false);
	});

	it('surfaces the failure message for the card', () => {
		const l = new RedemptionLedger();
		l.claim('req-1', ASK);
		l.fail('req-1', ASK, 'dial the manifest plane: timed out');
		expect(l.get('req-1')).toEqual({
			kind: 'failed',
			message: 'dial the manifest plane: timed out',
		});
	});
});

describe('ticketAnswersOurAsk — the unsolicited-dial gate', () => {
	const asks = { 'npub1owner|criterion': { nonce: 'n-abc' } };
	// Carrier 4 — an ask scoped to a THIRD-party author: we asked npub1C for npub1A's "criterion".
	const reServeAsks = { 'npub1C|npub1A|criterion': { nonce: 'n-rs' } };

	/** **An IP-exposure control.** Redemption dials the peer and the card fires on render, so a
	 *  ticket we did not provoke must never dial. */
	it('accepts only a ticket echoing the nonce we minted for this exact ask', () => {
		expect(ticketAnswersOurAsk(asks, 'npub1owner', 'criterion', 'n-abc')).toBe(true);
	});

	/** **The reason the nonce exists.** Matching on peer+slug alone is a STANDING authorization: the
	 *  peer satisfies it once, then mints unlimited fresh tickets — any request_id, and any node
	 *  address they choose — and we dial every one. `request_id` cannot fix it because the OWNER
	 *  mints that; only a value we generated can. */
	it('refuses a fresh ticket from a peer we HAVE asked, when the nonce does not match', () => {
		expect(ticketAnswersOurAsk(asks, 'npub1owner', 'criterion', 'attacker-chosen')).toBe(false);
	});

	it('refuses the wrong collection and the wrong peer', () => {
		expect(ticketAnswersOurAsk(asks, 'npub1owner', 'other', 'n-abc')).toBe(false);
		expect(ticketAnswersOurAsk(asks, 'npub1stranger', 'criterion', 'n-abc')).toBe(false);
	});

	/** Fails closed on every ambiguity. The cost of failing closed is one re-ask; the cost of failing
	 *  open is on-demand IP disclosure. */
	/** ⚠ The no-nonce-on-the-ticket case is enforced by the nonce EQUALITY, not by the explicit
	 *  `if (!ticketNonce)` guard above it — deleting that guard leaves this green. Noted so nobody
	 *  reads its presence as the enforcement. */
	it('refuses when the trace has not loaded, the ask predates the ruling, or the ticket carries no nonce', () => {
		expect(ticketAnswersOurAsk(null, 'npub1owner', 'criterion', 'n-abc')).toBe(false);
		expect(ticketAnswersOurAsk({}, 'npub1owner', 'criterion', 'n-abc')).toBe(false);
		// A pre-ruling ask has no stored nonce — it can no longer auto-dial.
		expect(
			ticketAnswersOurAsk({ 'npub1owner|criterion': {} }, 'npub1owner', 'criterion', 'n-abc'),
		).toBe(false);
		// A pre-ruling owner echoes nothing.
		expect(ticketAnswersOurAsk(asks, 'npub1owner', 'criterion', undefined)).toBe(false);
		expect(ticketAnswersOurAsk(asks, 'npub1owner', 'criterion', '')).toBe(false);
	});

	/** The lookup must not be satisfied by an INHERITED property.
	 *
	 *  Rewritten 2026-08-31 (audit). The previous version passed 'constructor' as a *slug*, which
	 *  could not fail under any implementation: the lookup key is composite (`npub|slug`, now
	 *  `npub|author|slug`) and no `Object.prototype` member name contains a `|`, so the map access
	 *  was always `undefined` whether or not the guard existed. Polluting the prototype with the
	 *  exact composite key is the only input shape that actually reaches the `hasOwnProperty`
	 *  check. */
	it('is not fooled by an inherited property on Object.prototype', () => {
		const proto = Object.prototype as unknown as Record<string, unknown>;
		const polluted = 'npub1owner|criterion';
		proto[polluted] = { nonce: 'n-abc' };
		try {
			expect(ticketAnswersOurAsk({}, 'npub1owner', 'criterion', 'n-abc')).toBe(false);
		} finally {
			delete proto[polluted];
		}
	});

	// ── Carrier 4 — the ask identity is (responder, author, slug, nonce) ────────────────────────
	// Peer C can re-serve a manifest peer A authored, so an ask is scoped by the author too, and a
	// re-serve ticket is redeemed on arrival exactly like an owner ticket — no more, no less.

	/** The happy path that did not exist before: we asked C for A's manifest, C re-served it, and the
	 *  ticket echoes the nonce we minted for exactly that ask. */
	it('a re-serve ticket from C for an ask scoped to author A matches', () => {
		expect(ticketAnswersOurAsk(reServeAsks, 'npub1C', 'criterion', 'n-rs', 'npub1A')).toBe(true);
	});

	/** **The whole point of the widening.** Without the author in the key, C's re-serve ticket would
	 *  collide with any ask of ours that happens to share (C, slug, nonce-equivalent) — the
	 *  cross-tenant collision. The nonce alone does not close it: the nonce is per-ask, not
	 *  per-author, so two asks to C for two different authors' "criterion" carry two nonces, and
	 *  nonce equality alone could still bind the WRONG ask's authorization to this ticket. */
	it('a ticket from C carrying the WRONG author does not match', () => {
		expect(ticketAnswersOurAsk(reServeAsks, 'npub1C', 'criterion', 'n-rs', 'npub1B')).toBe(false);
		// And the ask recorded under the authorless key must not be redeemed by a re-serve ticket —
		// that is the fallback the widening exists to close.
		expect(
			ticketAnswersOurAsk(asks, 'npub1owner', 'criterion', 'n-abc', 'npub1A'),
		).toBe(false);
	});

	/** **Backward compatibility, and it is load-bearing.** An ask recorded WITHOUT an author is
	 *  "the asked peer's own collection" — author === responder — so the owner-path ticket still
	 *  matches it. Breaking this would break every owner-path ask recorded before Carrier 4. */
	it('a legacy authorless ask still matches the owner-path ticket', () => {
		expect(ticketAnswersOurAsk(asks, 'npub1owner', 'criterion', 'n-abc', undefined)).toBe(true);
		expect(ticketAnswersOurAsk(asks, 'npub1owner', 'cookie', 'n-cookie', 'npub1owner')).toBe(
			false,
		); // no such ask
		// The widened self-author spelling of the SAME owner ask — one identity, two spellings.
		const selfAuthored = { 'npub1owner|npub1owner|criterion': { nonce: 'n-abc' } };
		expect(
			ticketAnswersOurAsk(selfAuthored, 'npub1owner', 'criterion', 'n-abc', 'npub1owner'),
		).toBe(true);
		expect(
			ticketAnswersOurAsk(selfAuthored, 'npub1owner', 'criterion', 'n-abc', undefined),
		).toBe(true);
	});

	/** The widened identity must never be EASIER to satisfy than the old one. Every fail-closed
	 *  property of the narrow gate, re-run against the widened key shapes. */
	it('fails closed on every ambiguity under the widened identity too', () => {
		// Re-serve: trace not loaded, empty map, missing stored nonce, empty/missing ticket nonce.
		expect(ticketAnswersOurAsk(null, 'npub1C', 'criterion', 'n-rs', 'npub1A')).toBe(false);
		expect(ticketAnswersOurAsk({}, 'npub1C', 'criterion', 'n-rs', 'npub1A')).toBe(false);
		expect(
			ticketAnswersOurAsk({ 'npub1C|npub1A|criterion': {} }, 'npub1C', 'criterion', 'n-rs', 'npub1A'),
		).toBe(false);
		expect(ticketAnswersOurAsk(reServeAsks, 'npub1C', 'criterion', undefined, 'npub1A')).toBe(
			false,
		);
		expect(ticketAnswersOurAsk(reServeAsks, 'npub1C', 'criterion', '', 'npub1A')).toBe(false);
		// Nonce mismatch on the exact widened key.
		expect(ticketAnswersOurAsk(reServeAsks, 'npub1C', 'criterion', 'attacker-chosen', 'npub1A')).toBe(
			false,
		);
		// Prototype pollution, widened: the third segment must not be satisfied by inheritance.
		expect(ticketAnswersOurAsk({}, 'npub1C', 'constructor', 'n-rs', 'toString')).toBe(false);
	});

	/** The ticket's author is part of the parsed binding, not just a gate argument — the page passes
	 *  `tk.authorNpub` from the parsed DM body into the gate, so the parser must carry it through
	 *  with the same strictness as `ask_nonce` (absent/empty ⇒ undefined ⇒ owner path). */
	it('parseTransportTicket carries the author through', () => {
		expect(parseTransportTicket(ticketBody({ author_npub: 'npub1A' }))).toMatchObject({
			authorNpub: 'npub1A',
		});
		expect(parseTransportTicket(ticketBody())).toMatchObject({ authorNpub: undefined });
		expect(parseTransportTicket(ticketBody({ author_npub: '' }))).toMatchObject({
			authorNpub: undefined,
		});
	});
});

describe('copy', () => {
	/* ⚠ 'the fulfil verb names the list, never the files or a download' — DELETED 2026-09-04
	 * (QURATOR-164). It pinned `SEND_FULL_LIST_LABEL`, the owner-side button's text. The owner
	 * deleted the verb ("nothing should show that"): public collections need no approval, so the card
	 * offers no action and the constant rendered nowhere. A test asserting the wording of copy that
	 * cannot reach a screen is vacuous, so the constant and this went together.
	 *
	 * MAS-INV-5 did NOT lose coverage — it gained. `mas-inv5-no-download.test.ts` now pins the
	 * stronger structural fact (the card contains no `<button` at all) plus the positive claim on
	 * `MANIFEST_AUTO_SENT_LINE`, which is copy that is actually rendered. */

	/** A failed redemption must say the request survives, because it does — the owner spends the
	 *  ticket only on this side's acknowledgement. Copy implying the user has to ask again would send
	 *  them to re-request something they can simply retry. */
	it('the failure line tells the user their request still stands', () => {
		expect(REDEEM_FAILED_LINE).toMatch(/still good/i);
	});
});
