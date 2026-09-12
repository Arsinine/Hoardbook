// Pure view-model for the unified per-peer read state (devtest #16) — replaces the three
// unsynchronized mechanisms (+layout.svelte's seenLayoutKeys, chat/+page.svelte's one-shot
// unreadCount.set(0), and its seenCounts snapshot) with a single derivation over the persisted
// per-peer watermark (`readWatermarks`, mirroring the backend `read_state.json`). No Svelte, no DOM,
// no Tauri → unit-testable in the node env.
//
// Scope: DM-only. Topic channels carry no unread signal anywhere in the app — this stays that way.

import type { ReceivedMessage } from './types.js';
import { manifestRequestHint, accessRequestHint } from './request-inbox.js';
import { transportTicketHint } from './transport-ticket.js';

/** QURATOR-204: manifest asks, access requests and transport tickets ride the real chat kind
 *  (kind 14) at the wire level — see chat.rs's QURATOR-187 kind guard, which only screens out the
 *  key-grant/private-listing kinds, not these. They already render as a hint bubble, not a chat
 *  message (chat/+page.svelte), so they must not bump the chat unread badge either. */
function isMachinePayload(content: string): boolean {
	return (
		manifestRequestHint(content) !== null ||
		accessRequestHint(content) !== null ||
		transportTicketHint(content) !== null
	);
}

/** Unread DM count per sender npub: messages strictly newer than that peer's watermark
 *  (`watermarks[from] ?? ''` — an absent watermark counts everything). Skips the caller's own
 *  sent-echoes (`from === ownNpub`) — those never count as unread. Also skips a recognized
 *  manifest-ask / access-request / transport-ticket payload (QURATOR-204) — those aren't chat. */
export function unreadByPeer(
	inbox: ReceivedMessage[],
	watermarks: Record<string, string>,
	ownNpub: string,
): Record<string, number> {
	const counts: Record<string, number> = {};
	for (const m of inbox) {
		if (m.from === ownNpub) continue;
		if (isMachinePayload(m.content)) continue;
		const watermark = watermarks[m.from] ?? '';
		if (m.sent_at > watermark) {
			counts[m.from] = (counts[m.from] ?? 0) + 1;
		}
	}
	return counts;
}

/** Total unread across every peer — the nav badge's count. */
export function totalUnread(byPeer: Record<string, number>): number {
	return Object.values(byPeer).reduce((sum, n) => sum + n, 0);
}

/** The newest `sent_at` among messages from `peerNpub` — the watermark value to advance to when a
 *  conversation is opened/read. `undefined` when there are no messages from that peer. */
export function latestFromPeer(inbox: ReceivedMessage[], peerNpub: string): string | undefined {
	let latest: string | undefined;
	for (const m of inbox) {
		if (m.from !== peerNpub) continue;
		if (!latest || m.sent_at > latest) latest = m.sent_at;
	}
	return latest;
}
