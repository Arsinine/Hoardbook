// QURATOR-335 — the per-collection full-list status (bottom-right of the Browse footer, beside
// "Metadata only. Hoardbook moves no files."). Owner-approved design, 2026-09-25:
// https://claude.ai/artifact/LouMiVCHe9FB8quSuBXVD7 — states 1–7.
//
// Pure: (ask record, in-flight byte progress, oversized flag, now) → what the row says. Every input
// is something the backend already persists or emits — the ask trace (`manifest_asks.json`, incl.
// the QURATOR-197 dial budget) and the `manifest-progress` event — so no new backend state exists
// just to drive this view.
//
// Colour rule (owner): GREEN while bytes move or the list is ready, RED when obstructed or failing,
// AMBER while simply waiting on the owner. Copy says "list", never the D-word (MAS-INV-5 / INV-4′).
//
// ⚠ QURATOR-159 ruling D7 still binds: tickets never expire, so there is NO "the owner never
// answered" timeout state. `asked` stays `asked` for as long as it takes. The red states key only
// on DIAL failures, which exist only after the owner answered with a ticket.

import type { ManifestAsk } from './api.js';

/** Mirrors `REDEEM_DIAL_CAP` (fetch_driver.rs): failed dials before the background fetch stops. */
export const FETCH_DIAL_CAP = 3;
/** Mirrors `REDEEM_DIAL_BACKOFF_BASE_SECS` = 2 × the 300 s poll; doubles per further failure. */
const FETCH_DIAL_BACKOFF_BASE_SECS = 600;

export type FetchTone = 'waiting' | 'ok' | 'bad';

export interface FetchStatus {
	tone: FetchTone;
	title: string;
	sub: string;
	/** 0–100 when a bar is drawn; `null` = no bar. */
	pct: number | null;
}

export interface ByteProgress { received: number; total: number }

/** The re-dial wait after `attempts` failures — `redeem_backoff_for`'s TS twin. */
function backoffSecs(attempts: number): number {
	if (attempts <= 0) return 0;
	return FETCH_DIAL_BACKOFF_BASE_SECS * 2 ** Math.min(attempts - 1, 16);
}

function hhmm(d: Date): string {
	return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function mb(bytes: number): string {
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** What the footer row says for this collection, or `null` for nothing to say (never asked, or
 *  the list already arrived — the imported note covers that). */
export function deriveFetchStatus(input: {
	oversized?: boolean;
	ask?: ManifestAsk;
	progress?: ByteProgress;
	now: Date;
}): FetchStatus | null {
	const { oversized, ask, progress, now } = input;
	// 7 — terminal, and it wins: nothing can ever be fetched, so no other state is honest.
	if (oversized) {
		return {
			tone: 'bad',
			title: 'Too large to send in full',
			sub: 'This list is over the 16 MB limit · showing the preview only',
			pct: null,
		};
	}
	// 3 — bytes are moving. A run that finished (received >= total) is no longer "in flight".
	if (progress && progress.total > 0 && progress.received < progress.total) {
		const pct = Math.min(100, Math.max(0, Math.round((progress.received / progress.total) * 100)));
		return {
			tone: 'ok',
			title: 'Receiving the full list',
			sub: `${mb(progress.received)} of ${mb(progress.total)} · ${pct}%`,
			pct,
		};
	}
	if (!ask || ask.spent) return null;
	const attempts = ask.dial_attempts ?? 0;
	// 5 — the background fetch stopped dialling this ask.
	if (attempts >= FETCH_DIAL_CAP) {
		return {
			tone: 'bad',
			title: "Couldn't get the full list",
			sub: `Gave up after ${FETCH_DIAL_CAP} attempts · asks again when the list changes`,
			pct: null,
		};
	}
	// 4 — a dial failed; the next one is scheduled.
	if (attempts > 0) {
		const retryAt = new Date(((ask.dial_last_fail_unix ?? 0) + backoffSecs(attempts)) * 1000);
		const sub = retryAt.getTime() > now.getTime()
			? `Attempt ${attempts} of ${FETCH_DIAL_CAP} · retrying at ${hhmm(retryAt)}`
			: `Attempt ${attempts} of ${FETCH_DIAL_CAP} · retrying on the next check`;
		return { tone: 'bad', title: "Couldn't reach the owner", sub, pct: null };
	}
	// 1 — asked; the owner has not answered yet (no timeout, ruling D7).
	const sent = Date.parse(ask.sent_at);
	return {
		tone: 'waiting',
		title: 'Asked for the full list',
		sub: Number.isNaN(sent) ? 'Waiting for the owner' : `Waiting for the owner · sent ${hhmm(new Date(sent))}`,
		pct: null,
	};
}
