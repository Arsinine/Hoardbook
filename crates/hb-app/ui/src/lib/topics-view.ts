//! Pure view-model for Topics (M11; spec §11) — the honest **membership-visibility consent** copy, the
//! join gate (F12), the topic-contact badge, the spoofable member-count display, and the "joining
//! unlocks no listings" note. No Svelte, no DOM, no Tauri → unit-testable in the node env.

import type { CachedPeer, ContactSource, ChannelPost, TopicLookup } from './types.js';
import { contactDisplayName } from './contact-display.js';

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

/** The Create-modal's primary action (devtest #11 — join-first): a same-name public Topic must not
 *  fork into a second, cryptographically distinct room. `lookup.exists` (an announce was found for
 *  the composed name) switches the action to **join** the existing room; `null` (no lookup yet, a
 *  private Topic, or an empty name) or `exists: false` keeps the default **create**. */
export interface PrimaryAction {
	label: string;
	mode: 'create' | 'join';
}

export function createPrimaryAction(lookup: TopicLookup | null): PrimaryAction {
	if (lookup?.exists) {
		return { label: `Join existing topic (${memberCountLabel(lookup.member_count_estimate)})`, mode: 'join' };
	}
	return { label: 'Create', mode: 'create' };
}

/** `topic_channel` returns posts newest-first (devtest #12; also feeds the Topics-page preview and
 *  discover ranking, which want newest-first — hb-net's contract stays as-is). Chat renders a
 *  channel like any other conversation, oldest at the top / newest at the bottom, so the render
 *  path sorts ascending here. Stable on ties (equal `ts`) — does not reorder same-second posts. */
export function sortChannelPostsAscending(posts: readonly ChannelPost[]): ChannelPost[] {
	return [...posts].sort((a, b) => a.ts - b.ts);
}

/** devtest #15 — resolve a `?topic=<id>` deep-link param (from the Topics-page "Open in Chat" link)
 *  against the loaded Topics list. `null` for an absent param or an id that isn't a joined Topic —
 *  the caller stays on the conversation list and can surface the not-joined/unknown case. */
export function resolveTopicParam<T extends { topic_id: string }>(
	topicId: string | null,
	topics: readonly T[],
): T | null {
	if (!topicId) return null;
	return topics.find((t) => t.topic_id === topicId) ?? null;
}

/** Dissolution is derived: a Topic with an empty roster has dissolved (the name frees up). */
export function isDissolved(rosterSize: number): boolean {
	return rosterSize <= 0;
}

/** The roster row label for a member npub — their petname/display-name when they're already a known
 *  contact, else a short npub (M13 W5 — replaces the bare-npub-only roster render). */
export function rosterLabel(npub: string, contacts: readonly CachedPeer[]): string {
	const contact = contacts.find((c) => c.npub === npub);
	return contactDisplayName(contact ?? { npub });
}

// ── W4: public Topic paths (fixed-root category + freeform sub-path) ──────────────────────────────

/** The six fixed-root categories a **public** Topic path must start with (mirrors `hb-core`'s
 *  `TOPIC_ROOTS`). The create form offers these as a picker, so a bad root is *unrepresentable* in
 *  the UI — and the backend re-validates authoritatively. */
export const TOPIC_ROOTS = ['video', 'audio', 'image', 'text', 'software', 'other'] as const;

/** Compose a public Topic path from the picked root + a freeform sub-path. Empty / slash-junk
 *  sub-segments are dropped; the result is `root` (just the category) or `root/sub/segments`. The
 *  backend re-normalizes (NFKC + lowercase + depth cap), so this is convenience, not the barrier. */
export function composeTopicPath(root: string, subPath: string): string {
	const subs = subPath.split('/').map((s) => s.trim()).filter(Boolean);
	return [root, ...subs].join('/');
}

/** Split a Topic name into its path segments (for the collapsible tree). */
export function splitTopicPath(name: string): string[] {
	return name.split('/').map((s) => s.trim()).filter(Boolean);
}

/** The sub-path label (everything below the root) for display under a root group; '' for a bare root. */
export function subPathLabel(name: string): string {
	return splitTopicPath(name).slice(1).join('/');
}

export interface TopicGroup<T> {
	root: string;
	topics: T[];
}

/** Group discovered Topics by their root category (the first path segment) for the collapsible tree
 *  (root category → sub-paths). Roots are ordered by [`TOPIC_ROOTS`]; an unexpected root sorts last.
 *  Within a root, input order is preserved (the backend already activity-ranks). */
export function groupTopicsByRoot<T extends { name: string }>(topics: T[]): TopicGroup<T>[] {
	const byRoot = new Map<string, T[]>();
	for (const t of topics) {
		const root = splitTopicPath(t.name)[0] ?? 'other';
		const bucket = byRoot.get(root);
		if (bucket) bucket.push(t);
		else byRoot.set(root, [t]);
	}
	const rank = (r: string) => {
		const i = (TOPIC_ROOTS as readonly string[]).indexOf(r);
		return i < 0 ? TOPIC_ROOTS.length : i;
	};
	return [...byRoot.entries()]
		.sort((a, b) => rank(a[0]) - rank(b[0]))
		.map(([root, ts]) => ({ root, topics: ts }));
}
