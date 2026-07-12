//! M15 W6 — pure helpers for the chat conversation list: last-message preview, which peers have any
//! history (so messageless contacts leave the list), and a compact relative timestamp. `now` is
//! injected so `relativeTime` is deterministically testable.

import type { ReceivedMessage } from './types.js';

export interface PeerPreview {
	text: string;      // "You: …" for outgoing; truncated to 48 chars, single line
	time: string;      // the ISO `sent_at` of the latest message (format with relativeTime)
	outgoing: boolean;
}

function truncate(s: string, n: number): string {
	const oneLine = s.replace(/\s+/g, ' ').trim();
	return oneLine.length > n ? oneLine.slice(0, n) + '…' : oneLine;
}

/** Latest message exchanged with `peerNpub` (inbound from them ∪ outbound to them) → preview.
 *  Null when there's no history with that peer. */
export function peerPreview(
	inbox: readonly ReceivedMessage[],
	sent: readonly ReceivedMessage[],
	peerNpub: string,
): PeerPreview | null {
	let best: { m: ReceivedMessage; outgoing: boolean } | null = null;
	let bestT = -Infinity;
	const consider = (m: ReceivedMessage, outgoing: boolean) => {
		const t = new Date(m.sent_at).getTime();
		if (t >= bestT) {
			bestT = t;
			best = { m, outgoing };
		}
	};
	for (const m of inbox) if (m.from === peerNpub) consider(m, false);
	for (const m of sent) if (m.to === peerNpub) consider(m, true);
	if (!best) return null;
	const b: { m: ReceivedMessage; outgoing: boolean } = best;
	return {
		text: (b.outgoing ? 'You: ' : '') + truncate(b.m.content, 48),
		time: b.m.sent_at,
		outgoing: b.outgoing,
	};
}

/** The set of peer npubs the user has any DM history with (either direction). */
export function peersWithHistory(
	inbox: readonly ReceivedMessage[],
	sent: readonly ReceivedMessage[],
): Set<string> {
	const s = new Set<string>();
	for (const m of inbox) s.add(m.from);
	for (const m of sent) s.add(m.to);
	return s;
}

/** Compact relative time: "now" / "2m" / "3h" / weekday ("Tue") within a week / "Mar 4" beyond. */
export function relativeTime(iso: string, now: Date): string {
	const then = new Date(iso).getTime();
	const diff = now.getTime() - then;
	const MIN = 60_000, HR = 3_600_000, DAY = 86_400_000;
	if (diff < MIN) return 'now';
	if (diff < HR) return `${Math.floor(diff / MIN)}m`;
	if (diff < DAY) return `${Math.floor(diff / HR)}h`;
	if (diff < 7 * DAY) return new Date(then).toLocaleDateString(undefined, { weekday: 'short' });
	return new Date(then).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}
