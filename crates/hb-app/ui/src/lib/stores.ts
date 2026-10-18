import { get, writable } from 'svelte/store';
import type { Collection, ContactSummary, DiscoveredTopic, IdentityInfo, Profile, ReceivedMessage, DmRequestView, TopicAnnounceSummary } from './types.js';

export const identity = writable<IdentityInfo | null>(null);
export const profile = writable<Profile | null>(null);
export const collections = writable<Collection[]>([]);
export const contacts = writable<ContactSummary[]>([]);

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
	fetch: () => Promise<ContactSummary[]>,
): Promise<ContactSummary[] | null> {
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

// ── QURATOR-379 — error toasts ──────────────────────────────────────────────────────
// Errors no longer share the single slot with success/Undo. They stack: one bottom-anchored
// column, oldest at the bottom, each new one directly above the newest existing one. Each error
// keeps its OWN 15s timer (from when IT appeared — a newer error never resets an older one's) and
// its own ✕. This reverses the devtest 2026-08-26 item-7 "errors are sticky" ruling, while
// keeping its reason: a short timeout was unreadable, and 15s answers that.
export const errorToasts = writable<{ id: number; text: string }[]>([]);
let errorSeq = 0;
// Per-id timers at module scope (a single shared timer would expire the wrong toast).
const errorTimers = new Map<number, ReturnType<typeof setTimeout>>();

/** The error lifetime. 15s: the owner's call — errors must be readable, and the old 3.5s was
 *  why they went sticky in the first place. */
export const TOAST_ERROR_MS = 15000;

/** How many errors the stack holds. When a 6th arrives the oldest is dropped. (Lane default, not
 *  the owner's number — picked so a burst cannot fill the window.) */
const ERROR_STACK_MAX = 5;

function scheduleError(id: number) {
	const prev = errorTimers.get(id);
	if (prev) clearTimeout(prev);
	errorTimers.set(id, setTimeout(() => removeError(id), TOAST_ERROR_MS));
}

function removeError(id: number) {
	const t = errorTimers.get(id);
	if (t) { clearTimeout(t); errorTimers.delete(id); }
	errorToasts.update((list) => list.filter((e) => e.id !== id));
}

/** Dismiss one error from the stack (its ✕). Safe to call with an unknown id. */
export function dismissError(id: number) {
	removeError(id);
}

function pushError(text: string) {
	// Same text already on screen: collapse — restart THAT toast's 15s and move it to the top.
	const dup = get(errorToasts).find((e) => e.text === text);
	if (dup) {
		errorToasts.update((list) => [...list.filter((e) => e.id !== dup.id), dup]);
		scheduleError(dup.id);
		return;
	}
	const id = ++errorSeq;
	errorToasts.update((list) => {
		// Cap: a 6th error evicts the oldest (and clears its timer, so it cannot fire later).
		if (list.length >= ERROR_STACK_MAX) {
			const oldest = list[0]!;
			const t = errorTimers.get(oldest.id);
			if (t) { clearTimeout(t); errorTimers.delete(oldest.id); }
			list = list.slice(1);
		}
		return [...list, { id, text }];
	});
	scheduleError(id);
}

// M22 W6: the toast timer is tracked so a second toast REPLACES the live one (chosen over queueing —
// a queue risks undoing the wrong operation, and a stale handler firing against changed state is the
// exact failure the tracker rules out). A replace clears the first toast's timer so it can't kill the
// second early. This is the single source of truth: both toast() and toastWithAction() route through it.
let toastTimer: ReturnType<typeof setTimeout> | undefined;
// Devtest 2026-08-26 item 7: success durations DOUBLED — 3500 and 6000 were too short to finish
// reading. Errors no longer use these at all — they live in the errorToasts stack (above) with
// their own TOAST_ERROR_MS lifetime.
const TOAST_MS = 7000;
const TOAST_ACTION_MS = 12000; // undo needs more time to be usable

function clearToastTimer() {
	if (toastTimer) { clearTimeout(toastTimer); toastTimer = undefined; }
}

/** Dismiss the single-slot toast (the ✕ on an error-kind action toast). Safe to call when nothing
 *  is showing. */
export function dismissToast() {
	clearToastTimer();
	toastMessage.set(null);
}

export function toast(text: string, kind: 'success' | 'error' = 'success') {
	if (kind === 'error') {
		pushError(text);
		return;
	}
	clearToastTimer();
	toastMessage.set({ text, kind });
	toastTimer = setTimeout(() => { toastMessage.set(null); toastTimer = undefined; }, TOAST_MS);
}

/** Toast with an optional action button (M22 W6 undo). When the action fires, the toast clears
 *  immediately and the timer is cancelled so the handler can't fire twice. When the toast expires,
 *  the action is discarded — no stale handler survives against changed state. A second toast (with or
 *  without action) REPLACES the live one rather than queueing. Errors with actions stay on the
 *  single slot (no production caller emits one today — kept for type parity, and an Undo-shaped
 *  escape hatch must always render). */
export function toastWithAction(
	text: string,
	action: { label: string; run: () => void },
	kind: 'success' | 'error' = 'success',
) {
	// An Undo always renders on the single slot — the error stack never displaces it.
	clearToastTimer();
	toastMessage.set({ text, kind, action: { label: action.label, run: () => {
		clearToastTimer();
		toastMessage.set(null);
		action.run();
	} } });
	if (kind === 'error') {
		toastTimer = setTimeout(() => { toastMessage.set(null); toastTimer = undefined; }, TOAST_ERROR_MS);
		return;
	}
	toastTimer = setTimeout(() => { toastMessage.set(null); toastTimer = undefined; }, TOAST_ACTION_MS);
}

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

/** QURATOR-145 (Topics W3) — the cross-mount directory cache: the last-known-good painted tree.
 *  A plain top-level `let` inside a `.svelte` file's `<script>` is re-initialized every component
 *  instantiation, so it does NOT survive leaving the Topics route and coming back — that only
 *  looks module-level. This module-level store does survive, and that is its entire job.
 *
 *  WHY ONLY NON-EMPTY WINS EVER GET WRITTEN: the QURATOR-83 bug (shipped 52ca0b2) cached an EMPTY
 *  Discover root for the whole session, and a cached NOTHING is indistinguishable from the feature
 *  being broken — it caused the owner's original "Discover finds nothing" report (QURATOR-80). An
 *  empty or FAILED answer is never written, so the next open always re-asks the relays (W2's
 *  auto-population is what makes a slightly-stale populated tree harmless: it is the landing
 *  screen, not something you had to go clicking for). "Degrade to last-known-good" is the other
 *  half: a FAILED fetch leaves whatever is on screen alone — never blank, never a confident
 *  "no Topics" over data we already had. */
export const topicDirectoryCache = writable<DiscoveredTopic[]>([]);

// (The downloads store + applyDownloadEvent reducer were removed in v0.9.6 and did NOT come back with
// M18's transport plane — that plane carries manifests only, INV-4′, so there is no download to track.)
