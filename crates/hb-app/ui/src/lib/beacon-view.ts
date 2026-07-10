// Presence-beacon health view logic (devtest #9 same-NAT diagnosis). Maps the backend
// `BeaconReport` to a per-relay status line in the Settings Relays surface — the beacon rides the
// same outbound-write path as every other relay publish (DMs/discovery), so a per-relay reject here
// is a generic canary for that write path, not a presence-only signal. Pure, unit-tested.

import type { BeaconReport } from './api';

/** A short relative-time label for a beacon event, e.g. "just now" / "2 min ago" / "3 h ago". */
export function relativeAgo(deltaSecs: number): string {
	if (deltaSecs < 60) return 'just now';
	const mins = Math.floor(deltaSecs / 60);
	if (mins < 60) return `${mins} min ago`;
	const hours = Math.floor(mins / 60);
	return `${hours} h ago`;
}

export interface BeaconLineView {
	text: string;
	tone: 'ok' | 'warn' | 'bad';
}

/** The beacon status line for one relay row. */
export function beaconLine(report: BeaconReport | null, url: string, nowSecs: number): BeaconLineView {
	if (!report || report.lastAttemptAt === 0) {
		return { text: 'beacon: not sent yet', tone: 'warn' };
	}
	const entry = report.relays.find((r) => r.url === url);
	if (entry) {
		if (entry.outcome === 'rejected') {
			return { text: `beacon failing: ${entry.reason ?? 'rejected'}`, tone: 'bad' };
		}
		return { text: `beacon: sent ${relativeAgo(nowSecs - report.lastSuccessAt)}`, tone: 'ok' };
	}
	if (report.lastError) {
		return { text: `beacon failing: ${report.lastError}`, tone: 'bad' };
	}
	return { text: 'beacon: not sent yet', tone: 'warn' };
}
