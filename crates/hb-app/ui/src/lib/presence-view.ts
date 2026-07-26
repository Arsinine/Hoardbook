// M17 W5 — the contact row tells the truth about two DIFFERENT clocks, and stops conflating them.
//
// The defect (owner devtest 2026-07-23, item 2): "offline contacts do not say how long since last
// seen, it just says 'seen just now' indefinitely." Root cause — the row rendered `last_fetched`
// under the word "seen". `last_fetched` is when *we* last polled, and every contacts refresh stamps
// it to now, so the label reported our own poll and said "just now" about someone gone for a week.
//
// So there are two strings, and they are labelled as what they are:
//   - **"checked {t}"** — OUR cache age, from `last_fetched` (W5.1: the word "seen" was the lie).
//   - **"Last seen {t}"** — THEIR presence, from a beacon's `created_at` (W5.2), which we learn
//     from the fresh-presence map the 60s online poll already fetches. No per-contact fan-out.
//
// Never-observed renders "Last seen — unknown", not "never": "never" asserts something about them,
// when all we can honestly claim is something about our own knowledge.

/** The presence freshness window — matches `online.rs::ONLINE_WINDOW_SECS` (10 min, Decision #12).
 *  A beacon inside it means "Online"; outside it, we render its age instead. */
export const PRESENCE_WINDOW_MS = 600_000;

/** How often the contact list re-reads the wall clock. The age must advance on its own — if it only
 *  moved when the 60s relay poll assigned new data, a failed poll or a hidden tab would freeze it,
 *  which is the original "seen just now forever" defect in a new form. Purely local: no network. */
export const PRESENCE_TICK_MS = 30_000;

export interface PresenceView {
	/** True while the newest beacon is inside the window → the row shows the Online pill, no age. */
	online: boolean;
	/** The offline age line, e.g. "Last seen 4h ago" / "Last seen — unknown". Empty when online (an
	 *  online contact needs no age — the pill already says it). */
	lastSeen: string;
}

/** Relative age of a past instant, coarse-grained. Deliberately has **no "just now" rung**: this
 *  ladder only ever labels a beacon already outside the 10-minute window, so "just now" could only
 *  ever be the old lie coming back. The floor is "{n}m ago". */
export function formatAge(ms: number): string {
	const mins = Math.max(1, Math.floor(ms / 60_000));
	if (mins < 60) return `${mins}m ago`;
	const hrs = Math.floor(mins / 60);
	if (hrs < 24) return `${hrs}h ago`;
	const days = Math.floor(hrs / 24);
	if (days < 30) return `${days}d ago`;
	return `${Math.floor(days / 30)}mo ago`;
}

/** Our cache age — "checked {t}" (W5.1). Same ladder, but "just now" is fine here: it is an honest
 *  statement about a poll that really did just happen. */
export function checkedLabel(lastFetched: string | null | undefined, now: number): string {
	if (!lastFetched) return 'checked never';
	const ms = now - new Date(lastFetched).getTime();
	if (!Number.isFinite(ms)) return 'checked never';
	if (ms < 120_000) return 'checked just now';
	return `checked ${formatAge(ms)}`;
}

/** Resolve a contact's presence state from the newest beacon timestamp we hold for them.
 *
 *  `seenAt` is the live fresh-set entry if the current poll saw them, else the persisted
 *  `last_presence`, else null. A future-dated stamp is treated as now (we don't trust a relay's
 *  clock to invent a negative age). */
export function presenceView(
	seenAt: string | null | undefined,
	now: number,
	windowMs: number = PRESENCE_WINDOW_MS,
): PresenceView {
	if (!seenAt) return { online: false, lastSeen: 'Last seen — unknown' };
	const ts = new Date(seenAt).getTime();
	if (!Number.isFinite(ts)) return { online: false, lastSeen: 'Last seen — unknown' };
	const age = Math.max(0, now - ts);
	if (age <= windowMs) return { online: true, lastSeen: '' };
	return { online: false, lastSeen: `Last seen ${formatAge(age)}` };
}

/** The newest presence stamp we hold for `npub`: this poll's fresh-set beats the persisted one.
 *  Returns null when we have never observed a beacon. */
export function newestSeen(
	npub: string,
	fresh: Map<string, string>,
	persisted: string | null | undefined,
): string | null {
	const live = fresh.get(npub);
	if (live && persisted) return new Date(live) >= new Date(persisted) ? live : persisted;
	return live ?? persisted ?? null;
}

/** Index an `OnlineCount.fresh` array by npub for row lookup. */
export function freshIndex(fresh: { npub: string; seen_at: string }[] | undefined): Map<string, string> {
	return new Map((fresh ?? []).map((f) => [f.npub, f.seen_at]));
}
