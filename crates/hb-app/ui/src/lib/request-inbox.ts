// Q7 — the stranger-DM Request inbox (M13 Part B; owner ruling): pure view-model logic (badge count,
// sort, preview truncation, the reply gate) so the Svelte component stays thin. No Svelte, no DOM, no
// Tauri — the backend (`dm_requests`) is the source of truth, this only renders it.

import type { DmRequestView } from './types.js';

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
 *  manifest-request DM shows its human hint instead of the raw JSON payload. */
export function requestPreview(r: DmRequestView, max = 80): string {
	const last = r.messages[r.messages.length - 1];
	const text = manifestRequestHint(last?.content ?? '') ?? last?.content ?? '';
	return text.length > max ? text.slice(0, max - 1) + '…' : text;
}

/** M16 W4 — the structured "get the rest" request a browser DMs to ask for a large collection's full
 *  manifest. Hoardbook neither auto-produces a manifest nor a ticket from this — a human decides. */
export interface ManifestRequest {
	slug: string;
	fingerprintSeen: string;
	teaserEventId?: string;
	/** Opaque to Hoardbook (neither minted nor validated) — the requester's Mascara pubkey, if any. */
	mascaraPubkey?: string;
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
		mascaraPubkey: typeof o.mascara_pubkey === 'string' ? o.mascara_pubkey : undefined,
	};
}

/** The light, human hint a manifest-request DM renders as ("Asking for the full list of …"), or null
 *  for an ordinary message. The hoarder then exports the manifest and tickets it in Mascara by hand. */
export function manifestRequestHint(content: string): string | null {
	const req = parseManifestRequest(content);
	return req ? `Asking for the full list of “${req.slug}”` : null;
}

/** No reply is possible until the sender becomes a contact (accepting the request adds them). */
export function canReply(isContact: boolean): boolean {
	return isContact;
}

export const REQUEST_EXPLAINER =
	"Requests are from people not in your contacts. They can't see whether you've read them. " +
	'Accepting adds the contact.';
