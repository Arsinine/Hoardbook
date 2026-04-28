import { writable } from 'svelte/store';
import type { CachedPeer, Collection, IdentityInfo, Profile, ReceivedMessage } from './types.js';

export const identity = writable<IdentityInfo | null>(null);
export const profile = writable<Profile | null>(null);
export const collections = writable<Collection[]>([]);
export const contacts = writable<CachedPeer[]>([]);

/** Messages received from the relay (inbox), fetched on the chat page. */
export const inboxMessages = writable<ReceivedMessage[]>([]);

/** Messages sent this session (in-memory; cleared on restart). */
export const sentMessages = writable<ReceivedMessage[]>([]);

export const toastMessage = writable<{ text: string; kind: 'success' | 'error' } | null>(null);

/** True once the layout's initial data fetch has completed. */
export const appReady = writable(false);

/** Count of messages received since the chat page was last opened. */
export const unreadCount = writable(0);

export function toast(text: string, kind: 'success' | 'error' = 'success') {
	toastMessage.set({ text, kind });
	setTimeout(() => toastMessage.set(null), 3500);
}
