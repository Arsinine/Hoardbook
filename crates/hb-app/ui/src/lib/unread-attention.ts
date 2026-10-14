// QURATOR-340 — the unread-attention trigger: one pure decision + its Tauri invoke wrapper.
//
// This lives OUTSIDE lib/api.ts deliberately: six existing tests mount +layout.svelte with a fixed
// vi.mock('$lib/api.js') factory (layout-listen-catch, error-toast-sticky, q81-window-chrome,
// q139-brand-github-link, layout-serving-notice, q100-picture-error-toast). A new export on
// $lib/api.js would be `undefined` under those mocks, and the layout's new $effect would call
// undefined() inside its effect — crashing every one of those mounts. A separate module keeps their
// mocks untouched: under them the REAL module loads, its isTauri guard rejects outside Tauri, and
// the layout's effect swallows the rejection (the same absorb-it discipline as the listen chains).
//
// The layout imports BOTH names from here (+layout.svelte, after the navChatCount derivation).

import { invoke as _invoke } from '@tauri-apps/api/core';

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	if (!isTauri) return Promise.reject(new Error(`Tauri not available (cmd: ${cmd})`));
	return _invoke<T>(cmd, args);
}

/** Pure view of one attention tick: `prev === undefined` is the COLD LOAD — the layout's first
 *  $effect run, which must never flash (owner ruling: fire only for messages that arrive while
 *  unfocused; a cold load is not a message arrival). `newArrival` is the navChatCount INCREASE —
 *  the "ONE flash per new message" edge. A decrease (watermarks advanced) is overlay-maintenance
 *  only. Equal counts: a no-change re-run, overlay-only refresh. */
export function attentionArgs(prev: number | undefined, next: number): {
	unread: number;
	newArrival: boolean;
} {
	return { unread: next, newArrival: prev !== undefined && next > prev };
}

/** Rust owns the focus decision (no JS window API, no new capability permission): the command
 *  reads `is_focused()` itself, paints/clears the Windows overlay icon, and flashes ONCE per new
 *  arrival, `Informational` never `Critical`. Errors are swallowed in Rust — but the promise can
 *  still REJECT outside Tauri (jsdom), so every caller must `.catch(() => {})` it. */
export function setUnreadAttention(unread: number, newArrival: boolean): Promise<void> {
	return invoke<void>('set_unread_attention', { unread, newArrival });
}
