//! Pure view-model for Topics (M11; spec §11) — the honest **membership-visibility consent** copy, the
//! join gate (F12), the topic-contact badge, the spoofable member-count display, and the "joining
//! unlocks no listings" note. No Svelte, no DOM, no Tauri → unit-testable in the node env.

import type { ContactSource } from './types.js';

/** Public-join consent: the visibility is the deal. Anyone who joins can see you are a member. */
export const PUBLIC_JOIN_CONSENT =
	'Joining is public: anyone who joins this Topic can see that you are a member (your npub is on ' +
	'the members-only roster, which any joiner can read). Fine for a pseudonymous interest — the same ' +
	'exposure class as your public teaser.';

/** Private-join consent: a durable members-only membership record exists — the §11 threat note,
 *  lifted verbatim in spirit. The join MUST be gated behind an explicit acknowledgment (F12). */
export const PRIVATE_JOIN_CONSENT =
	'A durable, members-only membership record exists for this private Topic — it persists (encrypted) ' +
	'on relays for as long as members keep it, scoped to the people you have been admitted alongside. ' +
	'Weigh it before joining a private Topic around a sensitive subject.';

/** Joining unlocks no listings (INV-2) — surfaced wherever a Topic is joined/shown. */
export const NO_UNLOCK_NOTE =
	'Joining a Topic does not unlock anyone’s collections — you get each member’s npub + public teaser ' +
	'only. Browsing their listings still needs their share code, exchanged one-to-one as normal.';

/** The consent copy to show before joining — private vs public. */
export function joinConsentCopy(isPrivate: boolean): string {
	return isPrivate ? PRIVATE_JOIN_CONSENT : PUBLIC_JOIN_CONSENT;
}

/** F12 — the join action may fire ONLY after an explicit acknowledgment of the visibility consent. */
export function canJoin(acknowledged: boolean): boolean {
	return acknowledged === true;
}

/** The topic-contact badge label — a `Topic`-sourced contact gets a distinct badge; a manual add gets
 *  none (it is the default, no badge needed). */
export function contactBadge(source: ContactSource | undefined): string | null {
	return source === 'Topic' ? 'Topic' : null;
}

/** The honest member-count display — approximate + spoofable, never authoritative (so it always reads
 *  as an estimate, never a hard number). */
export function memberCountLabel(estimate: number): string {
	const n = Math.max(0, Math.floor(estimate));
	return `~${n} member${n === 1 ? '' : 's'} (estimate)`;
}

/** Dissolution is derived: a Topic with an empty roster has dissolved (the name frees up). */
export function isDissolved(rosterSize: number): boolean {
	return rosterSize <= 0;
}
