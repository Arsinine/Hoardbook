import { get, writable } from 'svelte/store';
import type { CachedPeer, Collection, IdentityInfo, Profile, ReceivedMessage, DmRequestView, TopicAnnounceSummary } from './types.js';

export const identity = writable<IdentityInfo | null>(null);
export const profile = writable<Profile | null>(null);
export const collections = writable<Collection[]>([]);
export const contacts = writable<CachedPeer[]>([]);

/** QURATOR-93 — load-error flags for the layout-seeded stores. The layout's silent
 *  `.catch(() => {})` left a FAILED `get_collections`/`get_contacts` indistinguishable from an
 *  empty list, so Home/Contacts rendered their confident "No … yet" empty states on data they
 *  never got. True = the last load FAILED (render the error + Retry); a subsequent successful
 *  fetch sets false (a stale error never hides a good list — the QURATOR-80/85 rule, both ways).
 *  Pages that re-fetch (`getContacts` after add/unlock, etc.) set these through the same
 *  `loadCollectionsInto`/`loadContactsInto` helpers so every success clears its flag. */
export const collectionsLoadError = writable(false);
export const contactsLoadError = writable(false);

// Concern-1: overlapping loads (retry + poll + mutation refresh) raced applying whichever result
// landed LAST, not whichever was requested last — a slow retry could overwrite a fast poll's newer
// success (or newer error) after the fact. A module-scope generation counter per helper fixes it: a
// completing load applies its result (success OR error) only if no later call has started since.
let collectionsGen = 0;
let contactsGen = 0;

/** Fetch `get_collections` into the store, setting/clearing the load-error flag by outcome.
 *  Returns whether the load succeeded (unused by callers today; kept for symmetry with
 *  loadContactsInto, which returns the fresh list). A call superseded by a later `loadCollectionsInto`
 *  before it settles applies nothing — neither its result nor its error — and returns false. */
export async function loadCollectionsInto(
	fetch: () => Promise<Collection[]>,
): Promise<boolean> {
	const gen = ++collectionsGen;
	try {
		const fresh = await fetch();
		if (gen !== collectionsGen) return false; // superseded — a newer load already applied
		collections.set(fresh);
		collectionsLoadError.set(false);
		return true;
	} catch {
		if (gen !== collectionsGen) return false; // superseded — don't clobber a newer success
		collectionsLoadError.set(true);
		return false;
	}
}

/** Fetch `get_contacts` into the store, setting/clearing the load-error flag by outcome.
 *  Returns the fresh list, or null when the load failed (callers fall back to the stale store). A
 *  call superseded by a later `loadContactsInto` before it settles applies nothing and returns null. */
export async function loadContactsInto(
	fetch: () => Promise<CachedPeer[]>,
): Promise<CachedPeer[] | null> {
	const gen = ++contactsGen;
	try {
		const fresh = await fetch();
		if (gen !== contactsGen) return null; // superseded — a newer load already applied
		contacts.set(fresh);
		contactsLoadError.set(false);
		return fresh;
	} catch {
		if (gen !== contactsGen) return null; // superseded — don't clobber a newer success
		contactsLoadError.set(true);
		return null;
	}
}

/** Draft profile form that persists across navigation until saved/published or app closes. */
export const homeDraft = writable<Profile | null>(null);

/** Messages received from the relay (inbox), fetched on the chat page. */
export const inboxMessages = writable<ReceivedMessage[]>([]);

/** Messages sent this session. QURATOR-91: also seeded from the feed — `get_messages` now returns
 *  OWN sends (persisted at-rest on the send path), so every `inboxMessages` write merges its
 *  `from === myNpub` entries here. The thread renders inbox-from-peer UNION sent-to-peer, so an
 *  own-npub entry that stayed only in `inboxMessages` would never render. */
export const sentMessages = writable<ReceivedMessage[]>([]);

/** Quarantined stranger-DM Request buckets (Q7 — the message-requests pattern), refreshed alongside
 *  `inboxMessages` on the chat page's poll. */
export const dmRequests = writable<DmRequestView[]>([]);

/** A toast action — an optional button rendered inside the toast (M22 W6 undo). `run` is invoked
 *  once on click; the toast clears immediately after so a second click can't fire the handler twice. */
export interface ToastAction {
	label: string;
	run: () => void;
}

/** QURATOR-91 — seed `sentMessages` from a fresh inbox feed. The backend now returns OWN sends
 *  (persisted at-rest on the send path), but they arrive mixed into `inboxMessages`, which the
 *  conversation thread only reads peer-side (`from === peer`); the sent side reads `sentMessages`
 *  (`to === peer`). MERGE, never replace: handleSend appends the just-sent bubble directly, and a
 *  replace-shaped seed would flash it out on the next 3s poll until the feed echo landed. Dedup key
 *  is `from|sent_at|content` — the same message the feed returns and the session already holds is
 *  one bubble, not two (own sends carry no client-visible wrap id). */
export function seedSentFromFeed(feed: readonly ReceivedMessage[], myNpub: string) {
	if (!myNpub) return;
	sentMessages.update((prev) => {
		const keys = new Set(prev.map((m) => `${m.from}|${m.sent_at}|${m.to}|${m.content}`));
		const fresh = feed.filter((m) => m.from === myNpub && !keys.has(`${m.from}|${m.sent_at}|${m.to}|${m.content}`));
		return fresh.length ? [...prev, ...fresh] : prev;
	});
}

export const toastMessage = writable<{ text: string; kind: 'success' | 'error'; action?: ToastAction } | null>(null);

/** True once the layout's initial data fetch has completed. */
export const appReady = writable(false);

/** Set when the identity file exists but cannot be decrypted (e.g. DPAPI failure). */
export const identityLoadError = writable<string | null>(null);

/** Per-peer persisted last-read watermark (npub → RFC3339 timestamp), mirroring the backend
 *  `read_state.json` — the single source of truth the unread badge derives from (devtest #16:
 *  replaces the three unsynchronized mechanisms this used to be spread across). */
export const readWatermarks = writable<Record<string, string>>({});

/** devtest #2 — the background announcement poll's latest per-topic summaries, and the persisted
 *  per-topic seen watermarks (topic_id → newest seen ts) mirroring `announce_seen.json`. The Topics
 *  nav badge derives from both together (a topic is "unseen" when its latest_ts is past its watermark). */
export const topicAnnounceSummaries = writable<TopicAnnounceSummary[]>([]);
export const announceSeen = writable<Record<string, number>>({});

// M22 W6: the toast timer is tracked so a second toast REPLACES the live one (chosen over queueing —
// a queue risks undoing the wrong operation, and a stale handler firing against changed state is the
// exact failure the tracker rules out). A replace clears the first toast's timer so it can't kill the
// second early. This is the single source of truth: both toast() and toastWithAction() route through it.
let toastTimer: ReturnType<typeof setTimeout> | undefined;
// Devtest 2026-08-26 item 7 (owner follow-up): both success durations DOUBLED — 3500 and 6000 were
// too short to finish reading. Errors don't use these at all any more; they are sticky.
const TOAST_MS = 7000;
const TOAST_ACTION_MS = 12000; // undo needs more time to be usable

function clearToastTimer() {
	if (toastTimer) { clearTimeout(toastTimer); toastTimer = undefined; }
}

/** Devtest 2026-08-26 item 7 — an ERROR toast never auto-expires. Errors are the messages the user
 *  has to READ (the manifest-too-big one is three sentences and names a byte count and a menu path),
 *  and 3.5s is not enough to read them; the owner reported the text "disappears too quickly to be
 *  read". A sticky toast is dismissed by the ✕ the layout renders for it, via dismissToast(). Success
 *  toasts are unchanged: still TOAST_MS. */
export function isStickyToast(kind: 'success' | 'error'): boolean {
	return kind === 'error';
}

/** Dismiss the live toast (the ✕ on a sticky error). Safe to call when nothing is showing. */
export function dismissToast() {
	clearToastTimer();
	toastMessage.set(null);
}

/** A PLAIN success toast must not silently replace a sticky error the user hasn't dismissed — that
 *  would reintroduce the exact "it vanished before I could read it" failure by another route. Errors
 *  still replace errors (newest wins), matching the replace-not-queue rule above.
 *
 *  ⚠ This is consulted by `toast()` ONLY, never by `toastWithAction()`, and the asymmetry is the
 *  whole point. An action-bearing toast is not feedback — it is the Undo affordance, and for a
 *  drag-to-group move it is the ONLY undo path there is (registerUndo, contacts/+page.svelte and
 *  browse/+page.svelte). Suppressing it would silently destroy the user's ability to reverse a
 *  write they just made, which is a strictly worse outcome than an error toast being replaced —
 *  they can re-trigger the failing action to see the error again, but they cannot re-summon an Undo
 *  that never rendered. Text feedback yields to an unread error; an escape hatch does not. */
function blockedByStickyError(kind: 'success' | 'error'): boolean {
	if (kind === 'error') return false;
	return get(toastMessage)?.kind === 'error';
}

export function toast(text: string, kind: 'success' | 'error' = 'success') {
	if (blockedByStickyError(kind)) return;
	clearToastTimer();
	toastMessage.set({ text, kind });
	if (isStickyToast(kind)) return; // no timer — the ✕ is the only way out
	toastTimer = setTimeout(() => { toastMessage.set(null); toastTimer = undefined; }, TOAST_MS);
}

/** Toast with an optional action button (M22 W6 undo). When the action fires, the toast clears
 *  immediately and the timer is cancelled so the handler can't fire twice. When the toast expires,
 *  the action is discarded — no stale handler survives against changed state. A second toast (with or
 *  without action) REPLACES the live one rather than queueing. */
export function toastWithAction(
	text: string,
	action: { label: string; run: () => void },
	kind: 'success' | 'error' = 'success',
) {
	// No blockedByStickyError() here — see the note on that function. An Undo always renders.
	clearToastTimer();
	toastMessage.set({ text, kind, action: { label: action.label, run: () => {
		clearToastTimer();
		toastMessage.set(null);
		action.run();
	} } });
	if (isStickyToast(kind)) return; // sticky: the action stays live until the ✕ or the action itself
	toastTimer = setTimeout(() => { toastMessage.set(null); toastTimer = undefined; }, TOAST_ACTION_MS);
}

// (The downloads store + applyDownloadEvent reducer were removed in v0.9.6 and did NOT come back with
// M18's transport plane — that plane carries manifests only, INV-4′, so there is no download to track.)
