// M17 W7.1a — the ask must leave a trace (requester side). `request_manifest` really does build and
// publish the `{hb:"manifest_request",…}` DM, but `send_dm_inner` delivers to the recipient's inbox
// only — NO self-copy — so the ask leaves zero local trace: a toast fires and vanishes, the paywall
// block is unchanged, and the Chat tab shows nothing. From the owner's chair that is indistinguishable
// from a dead button.
//
// The fix is feedback, not transport. The ask is now persisted server-side (inside `request_manifest`,
// AFTER `send_dm_inner` resolves — a failed publish never records) as a small `manifest_asks.json` map
// keyed `"{npub}|{slug}"`. This helper derives the paywall block's asked-state PURELY from that map +
// the current clock, so:
//   - the asked-state survives a restart (read back from the store, not component-local state);
//   - "Ask again" is disabled inside the cooldown (60 min, mirroring the announce-cooldown idiom);
//   - a rejected publish leaves the un-asked state (the record is written only on success).
//
// Same rules as W7.1b's `manifest-fulfil.ts` + W4's `share-my-code.ts`: pure helpers, no DOM, no Tauri,
// no network — fully unit-testable. The route's `handleAskOwner` calls `requestManifest` then re-reads
// the map; this helper decides what the paywall block renders from the (map, npub, slug, now) tuple.

import type { ManifestAsk } from './api.js';

/** Re-ask cooldown in seconds. Mirrors `hb-core::ANNOUNCE_MIN_INTERVAL_SECS` (topic.rs:839): the
 *  announce-cooldown idiom — a pure `(lastSentAt, now) -> secondsRemaining`. 60 min. Relay citizenship:
 *  this can only ever *reduce* writes (a second click inside the window is a no-op). */
export const MANIFEST_ASK_COOLDOWN_SECS = 60 * 60;

/** Seconds remaining before the user may ask again, given their last `sent_at` for this `(npub, slug)`
 *  (`undefined` = never asked ⇒ 0). Pure + clock-injected so the countdown is deterministically
 *  testable. Mirrors `hb-core::announce_cooldown_remaining` (topic.rs:847) — saturating at 0 when the
 *  window has elapsed; clamps a clock rollback (now < sent_at) to the full window rather than
 *  underflowing. Does NOT throw on a malformed `sent_at` (defensive: a corrupted file must not blank
 *  the paywall block) — parse failure ⇒ 0 remaining. */
export function askCooldownRemaining(
	lastSentAt: string | undefined,
	now: Date,
): number {
	if (!lastSentAt) return 0;
	const then = Date.parse(lastSentAt);
	if (Number.isNaN(then)) return 0;
	const diffMs = now.getTime() - then;
	if (diffMs < 0) return MANIFEST_ASK_COOLDOWN_SECS; // clock rollback ⇒ full cooldown, never bypass
	const remainingSecs = MANIFEST_ASK_COOLDOWN_SECS - Math.floor(diffMs / 1000);
	return remainingSecs <= 0 ? 0 : remainingSecs;
}

/** The paywall block's asked-state. Pure derivation from (ask map, npub, slug, now). */
export type ManifestAskState =
	| { kind: 'unasked' }
	| { kind: 'asked'; sentAt: string; relative: string; cooldownRemaining: number; cooldownOver: boolean };

/** Read the ask map for `(npub, slug)` and derive the paywall block's state. `relative` is the
 *  compact "Asked {relative} — waiting for their reply" label, produced via `relativeTime` so it
 *  matches the rest of the app (chat-preview.ts). `cooldownRemaining` (seconds) drives the "Ask again"
 *  button's disabled state + tooltip; `cooldownOver` is the boolean the template reads.
 *
 *  `undefined` map / missing key / malformed sent_at ⇒ `unasked` (the paywall's default — never a
 *  phantom "Asked" from a corrupted or absent record). */
export function deriveManifestAskState(
	asks: Record<string, ManifestAsk> | undefined | null,
	npub: string,
	slug: string,
	now: Date,
): ManifestAskState {
	if (!asks) return { kind: 'unasked' };
	const entry = asks[manifestAskKey(npub, slug)];
	if (!entry) return { kind: 'unasked' };
	const then = Date.parse(entry.sent_at);
	if (Number.isNaN(then)) return { kind: 'unasked' }; // corrupted record ⇒ honest default
	const cooldownRemaining = askCooldownRemaining(entry.sent_at, now);
	return {
		kind: 'asked',
		sentAt: entry.sent_at,
		relative: relativeTimeForAsk(entry.sent_at, now),
		cooldownRemaining,
		cooldownOver: cooldownRemaining === 0,
	};
}

/** The on-disk key for an ask trace. Mirrors the Rust `manifest_ask_key` (store.rs). The pipe is
 *  unambiguous because npubs (bech32) and slugs (URL-safe charset) never contain `|`. */
export function manifestAskKey(npub: string, slug: string): string {
	return `${npub}|${slug}`;
}

// ── Copy (single source — the paywall block and the tests read from here) ─────────────────────

/** The muted asked-state line. "Asked {relative} — waiting for their reply." Relative is the compact
 *  chat-preview label (now / 2m / 3h / Tue / Mar 4). Never contains "Download" (MAS-INV-5). */
export const MANIFEST_ASKED_LINE = (relative: string) =>
	`Asked ${relative} — waiting for their reply.`;

/** The secondary "Ask again" button label. Disabled inside the cooldown; enabled after. */
export const MANIFEST_ASK_AGAIN_LABEL = 'Ask again';

/** The "Open chat" link copy — points the user where the reply will arrive (a DM). */
export const MANIFEST_OPEN_CHAT_LABEL = 'Open chat';

/** The tooltip on a cooldown-disabled "Ask again". Formats the remaining seconds as "Xm" / "Xh Ym".
 *  Mirrors how a reader would expect a countdown to read. */
/** How often the paywall block re-reads the clock. Both things it drives — the "Asked {relative}"
 *  label and the cooldown tooltip — are MINUTE-granular, so a 1s tick would wake the reactive graph
 *  60× per displayable change. 30s bounds the visible lag at half a minute; same call W5 made for
 *  the contact row (`PRESENCE_TICK_MS`). Purely local: no relay traffic. */
export const ASK_TICK_MS = 30_000;

export const MANIFEST_ASK_AGAIN_COOLDOWN_TIP = (remainingSecs: number) => {
	const m = Math.ceil(remainingSecs / 60);
	if (m < 60) return `Ask again in ${m}m`;
	const h = Math.floor(m / 60);
	const mm = m % 60;
	return mm === 0 ? `Ask again in ${h}h` : `Ask again in ${h}h ${mm}m`;
};

/** The inline muted reason shown when a publish failed. The button stays in its un-asked state —
 *  "Failure is loud" (spec): a failed ask must never render as "Asked". */
export const MANIFEST_ASK_FAILED_LINE = (reason: string) =>
	`Couldn’t send the request — ${reason}. You can try again.`;

// Compact relative time for the asked-state label. Mirrors `relativeTime` in chat-preview.ts:59 —
// "now" / "2m" / "3h" / weekday / "Mar 4". Kept local (not imported) so this helper has zero
// cross-file coupling beyond its own surface, matching how `chat-preview` and `manifest-fulfil` are
// each self-contained. The owner reads the "now" / "<N>m" form everywhere already.
function relativeTimeForAsk(iso: string, now: Date): string {
	const then = Date.parse(iso);
	if (Number.isNaN(then)) return '';
	const diff = now.getTime() - then;
	const MIN = 60_000, HR = 3_600_000, DAY = 86_400_000;
	if (diff < MIN) return 'now';
	if (diff < HR) return `${Math.floor(diff / MIN)}m`;
	if (diff < DAY) return `${Math.floor(diff / HR)}h`;
	if (diff < 7 * DAY) return new Date(then).toLocaleDateString(undefined, { weekday: 'short' });
	return new Date(then).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}
