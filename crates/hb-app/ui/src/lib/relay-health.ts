// Per-relay health view logic (M12 W1, Decision D). Maps the backend `RelayHealth` to a Settings
// relay-row view (label + tone dot) and derives the "why" hint shown when the online chip reads
// "–"/stale — so every failure mode no longer renders identically (HANDOVER #11). Pure, unit-tested.

import type { RelayHealth } from './api';

export interface RelayRowView {
	url: string;
	/** Human label for the relay's connection state. */
	label: string;
	/** Dot tone: ok (connected), warn (connecting), bad (unreachable/terminated). */
	tone: 'ok' | 'warn' | 'bad';
}

/** Map a backend `RelayHealth` to a Settings row view. */
export function relayRowView(h: RelayHealth): RelayRowView {
	switch (h.status) {
		case 'connected':
			return { url: h.url, label: 'Connected', tone: 'ok' };
		case 'connecting':
		case 'pending':
		case 'initialized':
			return { url: h.url, label: 'Connecting…', tone: 'warn' };
		case 'sleeping':
			return { url: h.url, label: 'Idle', tone: 'warn' };
		case 'banned':
			return { url: h.url, label: 'Banned', tone: 'bad' };
		default:
			// disconnected / terminated / anything unknown → unreachable.
			return { url: h.url, label: 'Unreachable', tone: 'bad' };
	}
}

/** A short "why" hint for a "–"/stale online chip, derived from per-relay health (Decision D). Empty
 *  when every relay is connected (no hint needed); otherwise names how many are unreachable so the
 *  user learns *why* the count is unknown rather than every failure looking identical. */
export function relayWhyHint(health: RelayHealth[]): string {
	if (health.length === 0) return 'No relays configured';
	const connected = health.filter((h) => h.connected).length;
	if (connected === health.length) return '';
	if (connected === 0) return 'No relay reachable';
	return `${health.length - connected} of ${health.length} relays unreachable`;
}
