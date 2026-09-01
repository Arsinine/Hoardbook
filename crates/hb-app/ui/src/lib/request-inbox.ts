// Q7 — the stranger-DM Request inbox (M13 Part B; owner ruling): pure view-model logic (badge count,
// sort, preview truncation, the reply gate) so the Svelte component stays thin. No Svelte, no DOM, no
// Tauri — the backend (`dm_requests`) is the source of truth, this only renders it.

import type { DmRequestView } from './types.js';
import { transportTicketHint } from './transport-ticket.js';
import { shortNpub } from './contact-display.js';

/** Shown beside the "Message requests" sidebar row — the number of quarantined stranger BUCKETS (not
 *  total messages: one badge count per distinct sender awaiting a decision). */
export function requestBadge(requests: DmRequestView[]): number {
	return requests.length;
}

/** Newest activity first — the same "most recently active" ordering the conversation list uses.
 *  Does not mutate the input. */
export function sortRequests(requests: DmRequestView[]): DmRequestView[] {
	return [...requests].sort((a, b) => b.last_message_at - a.last_message_at);
}

/** The bucket's LAST message, truncated for the row preview (an ellipsis marks truncation). A
 *  manifest-request DM shows its human hint instead of the raw JSON payload — and so does a M18
 *  transport ticket, which a stranger can send just as easily (it lands here, unredeemed, until its
 *  sender is accepted). Any structured DM body must have a hint here, or the row shows raw JSON. */
export function requestPreview(r: DmRequestView, max = 80): string {
	const last = r.messages[r.messages.length - 1];
	const content = last?.content ?? '';
	const text = manifestRequestHint(content) ?? transportTicketHint(content) ?? content;
	return text.length > max ? text.slice(0, max - 1) + '…' : text;
}

/** M16 W4 — the structured "get the rest" request a browser DMs to ask for a large collection's full
 *  manifest. Hoardbook neither auto-produces a manifest nor a ticket from this — a human decides. */
export interface ManifestRequest {
	slug: string;
	fingerprintSeen: string;
	teaserEventId?: string;
	/** The asker's nonce for this ask (owner ruling ①). The hoarder echoes it into the ticket; the
	 *  asker auto-dials only for a ticket carrying the nonce it stored. Absent from a request sent by
	 *  a client that predates the field — the resulting ticket then carries none and will not
	 *  auto-redeem, which is the intended fail-closed outcome. */
	askNonce?: string;
	/** **VESTIGIAL** — mirrors the Rust field of the same name (`chat.rs`). It carried the requester's
	 *  Mascara pubkey when a courier was going to move the file; that role is retired (2026-07-26) and
	 *  nothing writes or reads this. Kept because the request body is `wire_freeze`-pinned and the
	 *  field is already omitted from the wire in practice (owner ruling 2026-07-31, option b). */
	mascaraPubkey?: string;
	/** Carrier 4 (QURATOR-79) — the AUTHOR of the collection being asked for, when it is not the
	 *  asked peer's own: peer D asks peer C to re-serve a manifest peer A authored, from C's cache.
	 *  Absent means "the asked peer's own collection" — today's semantics exactly, which is why the
	 *  field is additive-optional and no wire discriminant moves (owner ruling 2026-08-30, same shape
	 *  as the ticket-side `authorNpub` in transport-ticket.ts). An empty string normalises to absent
	 *  here, so "present but blank" can never masquerade as a real author pin. */
	authorNpub?: string;
}

/** Detect the `{hb:"manifest_request",...}` JSON a browser sends as a DM. Returns the parsed request,
 *  or null for an ordinary chat message (any non-JSON / wrong-tag content). Pure. */
export function parseManifestRequest(content: string): ManifestRequest | null {
	let v: unknown;
	try {
		v = JSON.parse(content);
	} catch {
		return null;
	}
	if (typeof v !== 'object' || v === null) return null;
	const o = v as Record<string, unknown>;
	if (o.hb !== 'manifest_request' || typeof o.slug !== 'string') return null;
	return {
		slug: o.slug,
		fingerprintSeen: typeof o.fingerprint_seen === 'string' ? o.fingerprint_seen : '',
		teaserEventId: typeof o.teaser_event_id === 'string' ? o.teaser_event_id : undefined,
		askNonce:
			typeof o.ask_nonce === 'string' && o.ask_nonce !== '' ? o.ask_nonce : undefined,
		mascaraPubkey: typeof o.mascara_pubkey === 'string' ? o.mascara_pubkey : undefined,
		authorNpub:
			typeof o.author_npub === 'string' && o.author_npub !== '' ? o.author_npub : undefined,
	};
}

/** The light, human hint a manifest-request DM renders as ("Asking for the full list of …"), or null
 *  for an ordinary message. The hoarder answers it from the fulfil card in Chat — "Send the full
 *  list" over the transport plane (M18 W4), or the manifest export as the fallback. */
export function manifestRequestHint(content: string): string | null {
	const req = parseManifestRequest(content);
	if (!req) return null;
	// Carrier 4: a request naming a third-party author is a RE-SERVE ask — the peer is being asked
	// to hand over someone else's list from their cache, and the copy must say whose. The npub is
	// truncated the way the pane header renders one (shortNpub), so the hint stays one line.
	if (req.authorNpub) {
		return `Asking you to re-serve the full list of “${req.slug}” from ${shortNpub(req.authorNpub)}'s collection`;
	}
	return `Asking for the full list of “${req.slug}”`;
}

/** No reply is possible until the sender becomes a contact (accepting the request adds them). */
export function canReply(isContact: boolean): boolean {
	return isContact;
}

export const REQUEST_EXPLAINER =
	"Requests are from people not in your contacts. They can't see whether you've read them. " +
	'Accepting adds the contact.';
