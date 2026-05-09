import { writable } from 'svelte/store';
import type { CachedPeer, Collection, DownloadItem, IdentityInfo, Profile, ReceivedMessage } from './types.js';

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

export const toastMessage = writable<{ text: string; kind: 'success' | 'error' } | null>(null);

/** True once the layout's initial data fetch has completed. */
export const appReady = writable(false);

/** Count of messages received since the chat page was last opened. */
export const unreadCount = writable(0);

export function toast(text: string, kind: 'success' | 'error' = 'success') {
	toastMessage.set({ text, kind });
	setTimeout(() => toastMessage.set(null), 3500);
}

/** In-flight and recently completed downloads. */
export const downloads = writable<DownloadItem[]>([]);

interface ProgressEvent {
	id: number;
	filename: string;
	bytes_done: number;
	bytes_total: number;
	bytes_per_sec: number;
	status: DownloadItem['status'];
	error?: string;
}

/** Pure reducer — merges an incoming progress event into the download list. */
export function applyDownloadEvent(list: DownloadItem[], ev: ProgressEvent): DownloadItem[] {
	const idx = list.findIndex(d => d.id === ev.id);
	const patch: DownloadItem = {
		id: ev.id,
		filename: ev.filename,
		save_path: idx >= 0 ? list[idx].save_path : '',
		bytes_done: ev.bytes_done,
		bytes_total: ev.bytes_total,
		bytes_per_sec: ev.bytes_per_sec,
		status: ev.status,
		error: ev.error,
		started_at: idx >= 0 ? list[idx].started_at : Date.now(),
	};
	if (idx >= 0) {
		const next = [...list];
		next[idx] = patch;
		return next;
	}
	return [...list, patch];
}
