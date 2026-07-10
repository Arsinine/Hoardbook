import { writable } from 'svelte/store';
import type { CachedPeer, Collection, IdentityInfo, Profile, ReceivedMessage, DmRequestView } from './types.js';

export const identity = writable<IdentityInfo | null>(null);
export const profile = writable<Profile | null>(null);
export const collections = writable<Collection[]>([]);
export const contacts = writable<CachedPeer[]>([]);

/** Draft profile form that persists across navigation until saved/published or app closes. */
export const homeDraft = writable<Profile | null>(null);

/** Messages received from the relay (inbox), fetched on the chat page. */
export const inboxMessages = writable<ReceivedMessage[]>([]);

/** Messages sent this session (in-memory; cleared on restart). */
export const sentMessages = writable<ReceivedMessage[]>([]);

/** Quarantined stranger-DM Request buckets (Q7 — the message-requests pattern), refreshed alongside
 *  `inboxMessages` on the chat page's poll. */
export const dmRequests = writable<DmRequestView[]>([]);

export const toastMessage = writable<{ text: string; kind: 'success' | 'error' } | null>(null);

/** True once the layout's initial data fetch has completed. */
export const appReady = writable(false);

/** Set when the identity file exists but cannot be decrypted (e.g. DPAPI failure). */
export const identityLoadError = writable<string | null>(null);

/** Per-peer persisted last-read watermark (npub → RFC3339 timestamp), mirroring the backend
 *  `read_state.json` — the single source of truth the unread badge derives from (devtest #16:
 *  replaces the three unsynchronized mechanisms this used to be spread across). */
export const readWatermarks = writable<Record<string, string>>({});

export function toast(text: string, kind: 'success' | 'error' = 'success') {
	toastMessage.set({ text, kind });
	setTimeout(() => toastMessage.set(null), 3500);
}

// (The downloads store + applyDownloadEvent reducer were removed — file transfer moved to Mascara.)
