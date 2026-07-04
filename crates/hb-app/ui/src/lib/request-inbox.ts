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

/** The bucket's LAST message, truncated for the row preview (an ellipsis marks truncation). */
export function requestPreview(r: DmRequestView, max = 80): string {
	const last = r.messages[r.messages.length - 1];
	const text = last?.content ?? '';
	return text.length > max ? text.slice(0, max - 1) + '…' : text;
}

/** No reply is possible until the sender becomes a contact (accepting the request adds them). */
export function canReply(isContact: boolean): boolean {
	return isContact;
}

export const REQUEST_EXPLAINER =
	"Requests are from people not in your contacts. They can't see whether you've read them. " +
	'Accepting adds the contact.';
