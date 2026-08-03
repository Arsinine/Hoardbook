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

/** The two URL strings meeting here come from different places and are NOT byte-identical in
 *  general: the row's URL is the **configured** one (Settings / `default_relays.json`), while
 *  `report.relays[].url` is the **relay pool's** own rendering of it (`PublishOutcome`, via
 *  `RelayUrl::to_string`), which can differ in trailing slash and host case. Every other place that
 *  bridges those two sources already normalizes — `hb-net::RelayClient::relay_status` trims the
 *  trailing slash, `commands::settings::big_relay_overlaps_public` canonicalizes via `RelayUrl` —
 *  and a raw `===` here made a matched relay look unmatched, which the fall-through below then
 *  reported as "never sent". */
function sameRelay(a: string, b: string): boolean {
	const norm = (u: string) => u.trim().replace(/\/+$/, '').toLowerCase();
	return norm(a) === norm(b);
}

/** The loop-liveness breadcrumb line (v0.12.10 diagnostic). One line, rendered once near the relay
 *  rows: shows the current stage + wakeup count so a frozen report is distinguishable from a
 *  never-spawned loop. Pure — no tone logic, just a read-back of the backend's diagnostic fields. */
export function loopLine(report: BeaconReport | null): string {
	if (!report) return 'loop: no report';
	return `loop: ${report.stage || '—'} · wakeups ${report.loopWakeups}`;
}

/** The beacon status line for one relay row.
 *
 *  These states were one shared "not sent yet" string until 2026-08-02, which made the panel
 *  undiagnosable: a failed `beacon_status` invoke, a loop that never completed a cycle, and a relay
 *  the beacon simply never heard back from all read identically — and the app ships with no log
 *  subscriber and no devtools, so there was no other evidence anywhere. They now say which. */
export function beaconLine(report: BeaconReport | null, url: string, nowSecs: number): BeaconLineView {
	// No report at all: the `beacon_status` invoke has not succeeded yet (the caller swallows the
	// error and keeps the last-known report, so this persists only while it keeps failing).
	if (!report) {
		return { text: 'beacon: status unavailable', tone: 'warn' };
	}
	// A report exists but no cycle has ever recorded an attempt — the genuine "never sent" state.
	if (report.lastAttemptAt === 0) {
		return { text: 'beacon: not sent yet', tone: 'warn' };
	}
	const entry = report.relays.find((r) => sameRelay(r.url, url));
	if (entry) {
		if (entry.outcome === 'rejected') {
			return { text: `beacon failing: ${entry.reason ?? 'rejected'}`, tone: 'bad' };
		}
		return { text: `beacon: sent ${relativeAgo(nowSecs - report.lastSuccessAt)}`, tone: 'ok' };
	}
	if (report.lastError) {
		return { text: `beacon failing: ${report.lastError}`, tone: 'bad' };
	}
	// An attempt reached the relays, but this one neither accepted nor rejected it (dropped, or not
	// connected at publish time). Distinct from never having sent.
	return { text: 'beacon: no ack from this relay', tone: 'warn' };
}
