//! Pure view-model for Topics (M11; spec §11) — the honest **membership-visibility consent** copy, the
//! join gate (F12), the topic-contact badge, the spoofable member-count display, and the "joining
//! unlocks no listings" note. No Svelte, no DOM, no Tauri → unit-testable in the node env.

import type { CachedPeer, ContactSource, ChannelPost, AnnouncementView, TopicLookup, TopicAnnounceSummary } from './types.js';
import { contactDisplayName } from './contact-display.js';

/** Public-join consent: the visibility is the deal. Anyone who joins can see you are a member. */
export const PUBLIC_JOIN_CONSENT =
	'Joining is public. Anyone in this Topic can see you are a member, because your npub goes on a ' +
	'roster every joiner can read. That is the same visibility as your public profile.';

/** Private-join consent: a durable members-only membership record exists — the §11 threat note,
 *  lifted verbatim in spirit. The join MUST be gated behind an explicit acknowledgment (F12). */
export const PRIVATE_JOIN_CONSENT =
	'Private Topics keep an encrypted, members-only record of who joined. It stays on relays for as ' +
	'long as the Topic lives, visible to the people you were admitted alongside. Think twice before ' +
	'joining one around a sensitive subject.';

/** Joining unlocks no listings (INV-2) — surfaced wherever a Topic is joined/shown. */
export const NO_UNLOCK_NOTE =
	'Joining a Topic does not unlock anyone’s collections. You get each member’s npub and public ' +
	'profile only. Browsing their listings still needs their share code, person to person.';

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

/** The honest member-count display — spoofable by design (anyone can announce), so it reports what
 *  members have CLAIMED, never a measured figure (draft r1: "claimed" is honest without being
 *  alarming — "estimate" wrongly implied measurement imprecision rather than someone inflating it). */
export function memberCountLabel(estimate: number): string {
	const n = Math.max(0, Math.floor(estimate));
	return `${n} claimed`;
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

/** A single row in the rendered channel: either an ordinary post or a 📣 announcement. */
export type ChannelItem =
	| { kind: 'post'; ts: number; post: ChannelPost }
	| { kind: 'announce'; ts: number; announce: AnnouncementView };

/** devtest #6 — merge announcements into the ordinary post stream ordered by timestamp (ascending),
 *  instead of pinning them all above the posts. Announcements stay visually distinct at render (the
 *  📣 banner); only their position changes, so a broadcast now sits where it happened in the
 *  conversation. Stable: on an equal `ts` an announcement renders just before a post of the same
 *  second (announcements are listed first, and Array.sort is stable). */
export function interleaveChannel(
	posts: readonly ChannelPost[],
	announcements: readonly AnnouncementView[],
): ChannelItem[] {
	const items: ChannelItem[] = [
		...announcements.map((a): ChannelItem => ({ kind: 'announce', ts: a.ts, announce: a })),
		...posts.map((p): ChannelItem => ({ kind: 'post', ts: p.ts, post: p })),
	];
	return items.sort((a, b) => a.ts - b.ts);
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
 *  contact, else a short npub (M13 W5 — replaces the bare-npub-only roster render). devtest #3: the
 *  viewer is not in their own contacts, so a `self` (their own npub + published display_name) is
 *  matched first and rendered as "Name (you)" — never their bare npub. */
export function rosterLabel(
	npub: string,
	contacts: readonly CachedPeer[],
	self?: { npub: string; display_name?: string } | null,
): string {
	if (self && npub === self.npub) {
		const name = self.display_name?.trim();
		return name ? `${name} (you)` : 'You';
	}
	const contact = contacts.find((c) => c.npub === npub);
	return contactDisplayName(contact ?? { npub });
}

// ── devtest #2: topic-announcement alert (nav badge + toast) ──────────────────────────────────────

/** The joined Topics whose newest announcement is past the seen watermark — one entry per topic (an
 *  absent watermark counts as 0, so any announcement is unseen). Drives the Topics nav badge. */
export function unseenTopicAnnouncements(
	summaries: readonly TopicAnnounceSummary[],
	seen: Readonly<Record<string, number>>,
): TopicAnnounceSummary[] {
	return summaries.filter((s) => s.latest_ts > (seen[s.topic_id] ?? 0));
}

/** The Topics nav-badge count: how many joined Topics have an unseen announcement (topics, not messages). */
export function unseenAnnouncementCount(
	summaries: readonly TopicAnnounceSummary[],
	seen: Readonly<Record<string, number>>,
): number {
	return unseenTopicAnnouncements(summaries, seen).length;
}

/** Toast targets for one alert-poll tick: topics whose newest announcement is BOTH unseen AND newer
 *  than the previous poll's `baseline` (`topic_id → latest_ts`). Gating on the baseline means the
 *  first poll after launch (empty baseline) never toasts a backlog — those still badge via the seen
 *  watermark — and a steady announcement re-toasts only when a genuinely newer one lands. */
export function newlyArrivedAnnouncements(
	summaries: readonly TopicAnnounceSummary[],
	seen: Readonly<Record<string, number>>,
	baseline: Readonly<Record<string, number>>,
): TopicAnnounceSummary[] {
	return summaries.filter(
		(s) => s.topic_id in baseline && s.latest_ts > baseline[s.topic_id] && s.latest_ts > (seen[s.topic_id] ?? 0),
	);
}

/** The next poll's baseline (`topic_id → latest_ts`) from the current summaries — fed back into the
 *  next [`newlyArrivedAnnouncements`] call so only genuinely newer announcements re-toast. */
export function announcementBaseline(summaries: readonly TopicAnnounceSummary[]): Record<string, number> {
	const out: Record<string, number> = {};
	for (const s of summaries) out[s.topic_id] = s.latest_ts;
	return out;
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

// ── QURATOR-143 W1: lazy ranking (order by roster size, most popular first) ────────────────────────

/** The per-group draw cap (r4 ruling): a root group draws its ~25 most popular rows and states the
 *  remainder ("+N more under X"); joined rows are never truncated. The lazy ranker fetches counts
 *  ONLY for rows that will actually be drawn, so this cap is also the fetch bound per root. */
export const TOPIC_GROUP_DRAW_CAP = 25;

/** Round-robin interleave (QURATOR-143 W1, r4 owner ruling: "never spend all budget on one root").
 *  Takes per-root queues (root → ids, the ids in that root's draw order) and emits ONE flat list
 *  taking the head of each non-empty queue in turn — so with two roots pending, neither drains the
 *  other's slots: root A's first 8 ids cannot occupy all 8 concurrency slots before root B gets
 *  one. Removing the interleave (concatenating the queues instead) is exactly the mutation the
 *  round-robin test reds on. */
export function interleaveRoundRobin(queues: readonly (readonly string[])[]): string[] {
	const out: string[] = [];
	const rest = queues.map((q) => [...q]);
	// Loop until every queue is drained. `progress` guards a hypothetical all-empty input.
	for (let progressed = true; progressed; ) {
		progressed = false;
		for (const q of rest) {
			const head = q.shift();
			if (head !== undefined) {
				out.push(head);
				progressed = true;
			}
		}
	}
	return out;
}

/** Order a root group's rows by the lazily-fetched counts (most popular first), stable on ties and
 *  on not-yet-ranked rows (they keep their paint order, after every ranked row — an unfetched count
 *  is an unknown, not a zero). Pure: `counts` is a `topic_id → count` map as `topicRank` lands them. */
export function orderByMemberCount<T extends { topic_id: string; member_count_estimate: number | null }>(rows: readonly T[]): T[] {
	return [...rows].sort((a, b) => {
		const ca = a.member_count_estimate;
		const cb = b.member_count_estimate;
		// Unknown (null) sorts after known, whatever the known value is — never rendered as 0.
		if (ca === null && cb === null) return 0;
		if (ca === null) return 1;
		if (cb === null) return -1;
		return cb - ca; // most popular first (r4)
	});
}
