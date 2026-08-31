// M18 W4 slice 2 — the asker's half: recognising a transport ticket in the inbox, and the state of
// the one automatic action in this flow.
//
// **Why this is the only auto-firing path in Chat, and why that is not a contradiction of M17's
// "the app never auto-sends".** That ruling constrains the OWNER: Hoardbook must never produce or
// hand over a manifest without a human deciding. The asker's side is the mirror image — they already
// asked, and the owner has already clicked. Redeeming is the asker's own request completing, not a
// new act. The backend makes this structural rather than optional: `redeem_manifest_ticket` is the
// whole redemption path and there is deliberately **no deferred entry point** for a "Redeem later"
// button to bind to (owner ruling 2026-07-30 — a ticket is valid until redeemed precisely so nobody
// has to babysit it).
//
// So the card here reports, it does not gate. The one button it can show is **Retry**, and only
// after a failure — which is not deferral: the backend spends a ticket on the asker's acknowledgement
// alone, so a dial that never connected has cost nothing and re-attempting is the same immediate
// redemption, tried again.
//
// Where the result goes: `redeem_manifest_ticket` caches the verified envelope through the same
// `accept_manifest_bytes` path a file import uses, so Browse's existing cache-resolution upgrades the
// truncated teaser on its own. This module deliberately does NOT plumb a tree across tabs — the cache
// is already the hand-off, and inventing a second one would give the two paths different behaviour.

/** A transport ticket as it rides a DM body. Mirrors `hb_core::ticket::TransportTicket`; the fields
 *  this side needs are the bindings (which request, which collection) — `node_addr` stays opaque
 *  here exactly as it is in hb-core, and is passed back to the backend verbatim. */
export interface TransportTicket {
	requestId: string;
	slug: string;
	/** The asker's own nonce, echoed by the owner (owner ruling ①). Absent on a ticket minted before
	 *  the field existed — and absent never matches, so such a ticket will not auto-dial. */
	askNonce?: string;
	/** Unix seconds. Provenance and display only — **never an expiry input.** A ticket has no expiry
	 *  by design, and `hb_core::ticket` has a test that reds if one is added. */
	issuedAt: number;
	/** Carrier 4 — the AUTHOR of the collection being served. Absent on a ticket minted before the
	 *  field existed, and absent means **the issuer's own collection** (author === responder). The
	 *  field is additive and optional, so `TICKET_V` stays 1 (owner ruling 2026-08-30) — and absence
	 *  is not a downgrade: an authorless ticket can only ever answer an ask scoped to the issuer
	 *  itself, never a re-serve ask. */
	authorNpub?: string;
}

/** The `content.hb` discriminator for a ticket DM. Distinct from `manifest_request`'s, because the
 *  two travel in opposite directions and an inbox must never confuse them: a `manifest_request` is
 *  asker→owner and asks for something; a ticket is owner→asker and grants it. */
export const TICKET_TAG = 'transport_ticket';

/** Detect the `{hb:"transport_ticket",…}` JSON an owner DMs after clicking "Send the full list".
 *  Returns the parsed ticket, or null for any other message. Pure — no network, no invoke.
 *
 *  Deliberately strict about the discriminator AND the bindings: a body missing `request_id` or
 *  `slug` is not a ticket that merely lacks detail, it is something else. The backend re-checks all
 *  of this (`verify_shape`), so this is recognition, not validation — but recognising loosely here
 *  would render a card for a message that can only ever fail. */
export function parseTransportTicket(content: string): TransportTicket | null {
	let v: unknown;
	try {
		v = JSON.parse(content);
	} catch {
		return null;
	}
	if (typeof v !== 'object' || v === null) return null;
	const o = v as Record<string, unknown>;
	if (o.hb !== TICKET_TAG) return null;
	if (typeof o.request_id !== 'string' || o.request_id === '') return null;
	if (typeof o.slug !== 'string' || o.slug === '') return null;
	return {
		requestId: o.request_id,
		slug: o.slug,
		issuedAt: typeof o.issued_at === 'number' ? o.issued_at : 0,
		askNonce: typeof o.ask_nonce === 'string' && o.ask_nonce !== '' ? o.ask_nonce : undefined,
		authorNpub: typeof o.author_npub === 'string' && o.author_npub !== '' ? o.author_npub : undefined,
	};
}

/** The human hint a ticket DM renders as in a preview row, or null for an ordinary message — so a
 *  ticket never shows up as raw JSON in a conversation list (the same treatment
 *  `manifestRequestHint` gives a request). */
export function transportTicketHint(content: string): string | null {
	const t = parseTransportTicket(content);
	return t ? `Sent you the full list of “${t.slug}”` : null;
}

/** The redemption's lifecycle, as the card renders it.
 *
 *  `unsolicited` is the state a ticket lands in when we have **no local record of asking** that peer
 *  for that collection — see [`wasAsked`]. It is inert and dials nothing. */
export type RedemptionState =
	| { kind: 'redeeming' }
	| { kind: 'done' }
	| { kind: 'failed'; message: string }
	| { kind: 'unsolicited' }
	/** The local ask trace has not loaded (or its read failed), so we cannot yet tell a solicited
	 *  ticket from an unsolicited one. Distinct from `unsolicited` on purpose: it is recoverable, it
	 *  says so, and it must not accuse the sender of something they did not do. */
	| { kind: 'unverified' };

/** Does this ticket answer an ask **we** made? Compares the ticket's echoed nonce against the one
 *  stored in the local ask trace for `(responder, author, slug)`.
 *
 *  **This is an IP-exposure control, and the nonce is what makes it one** (owner ruling ①,
 *  2026-07-31). Redeeming *dials the peer*, so it reveals our address; the card fires on render.
 *  The earlier version of this gate asked only "did I ever ask this peer for this collection?" —
 *  which the peer can satisfy once and then exploit forever, minting fresh tickets with any
 *  `request_id` and **any node address of their choosing** and having us dial each one. `request_id`
 *  cannot close that: the OWNER mints it, so it proves nothing to us. Only a value we generated can.
 *
 *  Carrier 4 widens the identity from `(responder, slug, nonce)` to `(responder, author, slug,
 *  nonce)`: peer C can re-serve a manifest that peer A authored, so the ask is scoped by the author
 *  too. A ticket with no `author_npub` means **the issuer's own collection** — the lenient legacy
 *  reading, and the reason an authorless ticket can only ever answer an owner-path ask, never a
 *  re-serve one: absence is not a downgrade because there is no weaker behaviour to fall into.
 *
 *  Fails closed on every ambiguity — trace not loaded, no stored nonce (a pre-ruling ask), no nonce
 *  on the ticket (a pre-ruling owner), or a mismatch. The cost of failing closed is one re-ask; the
 *  cost of failing open is on-demand IP disclosure.
 */
export function ticketAnswersOurAsk(
	asks: Record<string, { nonce?: string }> | null,
	npub: string,
	slug: string,
	ticketNonce: string | undefined,
	ticketAuthor?: string,
): boolean {
	// Not loaded yet, or its read failed. Never read as "we asked" — a slow load must not become an
	// open auto-dial window.
	if (!asks) return false;
	// Redundant with the equality below (`'n-abc' === undefined` is already false) and kept
	// deliberately: it states the fail-closed intent at the boundary, and it holds if `stored` is ever
	// loosened. Deleting it does NOT redden the suite — verified — so do not read its presence as the
	// thing enforcing this case.
	if (!ticketNonce) return false;
	// Carrier 4 — normalize the ticket's author. Absent means the ISSUER'S OWN collection, not "any
	// collection the issuer cares to re-serve": the author is part of the ask's identity on BOTH
	// sides of the trace.
	const author = ticketAuthor && ticketAuthor !== '' ? ticketAuthor : npub;
	const candidates =
		author === npub
			? // Owner path: the same ask may be recorded under the pre-Carrier-4 authorless key OR the
				// widened self-author spelling — two spellings of one identity, and each is still
				// nonce-gated below, so trying both cannot widen what matches.
				[`${npub}|${slug}`, `${npub}|${npub}|${slug}`]
			: // Re-serve path: scoped to THIS author ONLY. It must never fall back to the authorless
				// key — that fallback IS the cross-tenant collision (ask for A's manifest, redeem on a
				// nonce minted for a different ask of ours, or the reverse).
				[`${npub}|${author}|${slug}`];
	for (const key of candidates) {
		if (!Object.prototype.hasOwnProperty.call(asks, key)) continue;
		const stored = asks[key]?.nonce;
		// A pre-ruling ask carries no nonce and can no longer auto-dial — it matches nothing.
		if (stored && stored === ticketNonce) return true;
	}
	return false;
}

/** The identity of one ASK — the unit an authorization is actually scoped to.
 *
 *  Not the `request_id`: the OWNER mints that, so a peer can produce as many distinct ones as it
 *  likes. Keyed on what *we* control.
 */
export function askIdentity(npub: string, slug: string, nonce: string): string {
	return `${npub}|${slug}|${nonce}`;
}

/** The redemption ledger.
 *
 *  Two levels of key, and the difference is the whole point:
 *
 *  - **per `request_id`** — the render-idempotence problem. The same DM re-renders on every 3 s poll
 *    and is re-read from cache across restarts; without a once-only claim a *successful* redemption
 *    would dial in a loop and show "already redeemed" errors for a success.
 *  - **per ASK** — the authorization problem. **Claiming only per request id was exploitable:** a peer
 *    that has seen one valid nonce can mint N tickets, each with a different owner-chosen
 *    `request_id` *and a different node address*, all echoing that one nonce. Each passes the nonce
 *    gate, each takes its own request-id slot, and all N dial before any of them completes — one ask
 *    turned into arbitrary concurrent dials to endpoints of the peer's choosing. So an ask admits
 *    **one in-flight redemption**, and once one succeeds the ask is **spent for the session**.
 *
 *  `#asksSpent` is deliberately independent of the backend's durable consume. If that write fails
 *  (disk full, permissions) the reusable authorization would otherwise come straight back; this
 *  keeps it closed until at least a restart, and a restart requires a fresh ask to re-authorize.
 */
export class RedemptionLedger {
	#states = new Map<string, RedemptionState>();
	#asksInFlight = new Set<string>();
	#asksSpent = new Set<string>();

	/** Claim `requestId` for a first attempt against `ask`. False when this ticket is already known,
	 *  when another ticket for the same ask is already redeeming, or when the ask is spent. */
	claim(requestId: string, ask: string): boolean {
		if (this.#states.has(requestId)) return false;
		if (this.#asksSpent.has(ask)) return false;
		if (this.#asksInFlight.has(ask)) return false;
		this.#asksInFlight.add(ask);
		this.#states.set(requestId, { kind: 'redeeming' });
		return true;
	}

	/** Claim for a retry after a failure. Only a `failed` ticket may be retried, and only while its
	 *  ask is neither spent nor otherwise in flight — so retry cannot become a second concurrent dial
	 *  any more than a first claim can. */
	claimRetry(requestId: string, ask: string): boolean {
		if (this.#states.get(requestId)?.kind !== 'failed') return false;
		if (this.#asksSpent.has(ask)) return false;
		if (this.#asksInFlight.has(ask)) return false;
		this.#asksInFlight.add(ask);
		this.#states.set(requestId, { kind: 'redeeming' });
		return true;
	}

	succeed(requestId: string, ask: string): void {
		this.#asksInFlight.delete(ask);
		// Spent for the session — one ask, one delivery, regardless of what the durable trace does.
		this.#asksSpent.add(ask);
		this.#states.set(requestId, { kind: 'done' });
	}

	fail(requestId: string, ask: string, message: string): void {
		// Released, not spent: a dial that failed cost nothing and stays retryable.
		this.#asksInFlight.delete(ask);
		this.#states.set(requestId, { kind: 'failed', message });
	}

	get(requestId: string): RedemptionState | undefined {
		return this.#states.get(requestId);
	}

	/** Whether this ask has already been answered in this session. */
	isSpent(ask: string): boolean {
		return this.#asksSpent.has(ask);
	}
}

// ── Copy (single source — the chat route and the tests read from here) ───────────────────────

/** The owner-side button. The verb is "send", not "download": what crosses is the **list**, and
 *  saying so is the difference between this and the affordance MAS-INV-5 forbids. */
export const SEND_FULL_LIST_LABEL = 'Send the full list';

/** Owner-side success. Names what was sent and what was not — the asker gets the listing, and the
 *  files stay where they are (INV-4′). */
export const SEND_FULL_LIST_TOAST = (slug: string) =>
	`Sent the full list of “${slug}”. Your files stay put.`;

/** Shown on the fulfil card next to the send button. Says the thing owner ruling ② made explicit:
 *  what crosses is the collection **as it stands when they fetch it**, not a frozen copy of what you
 *  are looking at now. Tickets do not expire, so that can be later than you expect. */
export const SEND_FULL_LIST_CURRENT_TREE =
	"They'll get the list as it stands when they fetch it, not a copy of it right now.";

/** Owner-side: export is the standing fallback, and stays reachable whether or not the transport
 *  works. Shown beside the send button, not instead of it. */
export const SEND_FULL_LIST_FALLBACK = 'Or export the file and hand it over yourself.';

/** Asker-side, while the dial and fetch are in flight. */
export const REDEEMING_LINE = (slug: string) => `Fetching the full list of “${slug}”…`;

/** Asker-side success. Points at Browse because that is where the upgraded tree appears — the
 *  redemption caches the verified envelope and Browse's existing cache resolution picks it up. */
export const REDEEMED_LINE = (slug: string) =>
	`The full list of “${slug}” arrived. Open it in Browse.`;

/** Asker-side failure. Says the ticket survives, because it does: the owner spends it only on this
 *  side's acknowledgement, so a failed attempt costs nothing and Retry is honest. */
export const REDEEM_FAILED_LINE = 'Couldn’t fetch it. The sender may be offline. Your request is still good.';

/** The retry button. Not a "redeem later" affordance: it appears only after a failure. */
export const REDEEM_RETRY_LABEL = 'Try again';

/** Asker-side, for a ticket we have no record of asking for. Says what it is and what was NOT done —
 *  the sender learns nothing, and we did not connect to them. */
/** Shown while the local ask trace is unavailable. Fail-closed like `unsolicited` — nothing is
 *  fetched — but recoverable and honestly labelled: the problem is on this side. */
export const UNVERIFIED_LINE =
	"Couldn't check your sent requests, so nothing was fetched yet. It retries automatically.";

export const UNSOLICITED_LINE =
	"They sent a full list you didn't ask for, so nothing was fetched. If you want it, request it from their collection in Browse.";
