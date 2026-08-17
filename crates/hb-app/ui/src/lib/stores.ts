import { writable } from 'svelte/store';
import type { CachedPeer, Collection, IdentityInfo, Profile, ReceivedMessage, DmRequestView, TopicAnnounceSummary } from './types.js';

export const identity = writable<IdentityInfo | null>(null);
export const profile = writable<Profile | null>(null);
export const collections = writable<Collection[]>([]);
export const contacts = writable<CachedPeer[]>([]);

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
const TOAST_MS = 3500;
const TOAST_ACTION_MS = 6000; // undo needs more time to be usable

function clearToastTimer() {
	if (toastTimer) { clearTimeout(toastTimer); toastTimer = undefined; }
}

export function toast(text: string, kind: 'success' | 'error' = 'success') {
	clearToastTimer();
	toastMessage.set({ text, kind });
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
	clearToastTimer();
	toastMessage.set({ text, kind, action: { label: action.label, run: () => {
		clearToastTimer();
		toastMessage.set(null);
		action.run();
	} } });
	toastTimer = setTimeout(() => { toastMessage.set(null); toastTimer = undefined; }, TOAST_ACTION_MS);
}

// (The downloads store + applyDownloadEvent reducer were removed in v0.9.6 and did NOT come back with
// M18's transport plane — that plane carries manifests only, INV-4′, so there is no download to track.)
