// @vitest-environment jsdom
// QURATOR-340 — the layout's unread-attention $effect, MOUNTED (P-4: try mounting first).
//
// Wiring under test: navChatCount ($derived over the real stores) drives exactly ONE
// setUnreadAttention(unread, newArrival) per change, with newArrival=true only on an INCREASE,
// and NOTHING on the cold load (prev undefined — a rehydrated unread pile is not an arrival).
// The focus gate itself lives in Rust (commands/window.rs::attention_plan) and is pinned there.
//
// $lib/api.js is mocked exactly like layout-listen-catch.test.ts (the layout's other calls);
// $lib/unread-attention.js keeps its REAL attentionArgs via importOriginal while setUnreadAttention
// becomes a spy — the same module the layout imports, so the wiring is real.
//
// MUTATION PROBE (CLAUDE.md §9 / P-10), applied by line number with sed, red confirmed by run:
// MUTATION M1 — delete the cold-load guard line `if (prevChatCount === undefined) {` block opener
//   (and its return) in +layout.svelte so the first run falls through and invokes: the cold-load
//   test below reds ("expected spy not to have been called"; the sequence test's call #1 shifts).
// MUTATION M2 — in src/lib/unread-attention.ts, change `newArrival: prev !== undefined && next > prev`
//   to `newArrival: next > (prev ?? -1)` on that line: the pure-helper test reds —
//   attentionArgs(undefined, 0) would return newArrival true only when next>prev... with prev
//   undefined the ?? branch makes 0 > -1 → true, breaking "cold load is never an arrival".
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { readable, writable } from 'svelte/store';
import { render, cleanup } from '@testing-library/svelte';
import { tick } from 'svelte';

vi.mock('$app/stores', () => ({ page: readable({ url: new URL('http://localhost/') }) }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '0.17.0') }));
// WindowControls dereferences window.__TAURI_INTERNALS__.metadata through getCurrentWindow() on
// mount — stub the module (same note as layout-listen-catch.test.ts).
vi.mock('@tauri-apps/api/window', () => ({
	getCurrentWindow: () => ({
		isMaximized: async () => false,
		onResized: async () => () => { },
		minimize: async () => { },
		toggleMaximize: async () => { },
		close: async () => { },
	}),
}));
vi.mock('@tauri-apps/api/event', () => ({
	listen: vi.fn(async () => () => { }),
}));
vi.mock('$lib/api.js', () => ({
	getIdentity: vi.fn(async () => null),
	getProfile: vi.fn(async () => null),
	getCollections: vi.fn(async () => []),
	getContacts: vi.fn(async () => []),
	getMessages: vi.fn(async () => []),
	getReadState: vi.fn(async () => ({})),
	topicAnnouncements: vi.fn(async () => []),
	topicAnnounceSeen: vi.fn(async () => ({})),
	openRepoPage: vi.fn(),
}));
// Real attentionArgs (via importOriginal); the invoke wrapper becomes a spy. The spy is hoisted
// because vi.mock factories are hoisted above const declarations.
const setUnreadAttention = vi.hoisted(() => vi.fn(async () => { }));
vi.mock('$lib/unread-attention.js', async (importOriginal) => ({
	...(await importOriginal<Record<string, unknown>>()),
	setUnreadAttention,
}));

import Layout from './+layout.svelte';
import { attentionArgs } from '$lib/unread-attention.js';
import { identity, inboxMessages, readWatermarks } from '$lib/stores.js';

const OWN = 'npub1ownnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn';
const PEER = 'npub1peer';

function msg(sentAt: string) {
	return { from: PEER, to: OWN, content: 'hello', sent_at: sentAt };
}

beforeEach(() => {
	vi.clearAllMocks();
	identity.set({ npub: OWN } as never);
	inboxMessages.set([]);
	readWatermarks.set({});
});

afterEach(() => {
	cleanup();
	inboxMessages.set([]);
	readWatermarks.set({});
	identity.set(null);
});

describe('q340 the layout drives the OS attention exactly once per navChatCount change', () => {
	it('cold load records the baseline and fires nothing', async () => {
		render(Layout);
		await tick();
		expect(setUnreadAttention, 'the cold load must not invoke the command').not.toHaveBeenCalled();
	});

	it('increases flash, stale counts overlay-only, watermark clears: the full sequence', async () => {
		render(Layout);
		await tick();
		expect(setUnreadAttention).not.toHaveBeenCalled();

		inboxMessages.set([msg('2026-10-14T10:00:00Z')]);
		await tick();
		expect(setUnreadAttention).toHaveBeenCalledTimes(1);
		expect(setUnreadAttention).toHaveBeenNthCalledWith(1, 1, true);

		inboxMessages.set([msg('2026-10-14T10:00:00Z'), msg('2026-10-14T11:00:00Z')]);
		await tick();
		expect(setUnreadAttention).toHaveBeenCalledTimes(2);
		expect(setUnreadAttention).toHaveBeenNthCalledWith(2, 2, true);

		// Watermark between the two messages: one left unread — a DECREASE is not an arrival.
		readWatermarks.set({ [PEER]: '2026-10-14T10:30:00Z' });
		await tick();
		expect(setUnreadAttention).toHaveBeenCalledTimes(3);
		expect(setUnreadAttention).toHaveBeenNthCalledWith(3, 1, false);

		// Watermark past everything: unread 0 — Rust clears the badge even while unfocused.
		readWatermarks.set({ [PEER]: '2026-10-14T12:00:00Z' });
		await tick();
		expect(setUnreadAttention).toHaveBeenCalledTimes(4);
		expect(setUnreadAttention).toHaveBeenNthCalledWith(4, 0, false);
	});

	it('attentionArgs: cold load never an arrival; increase yes; decrease/equal no', () => {
		expect(attentionArgs(undefined, 0)).toEqual({ unread: 0, newArrival: false });
		expect(attentionArgs(undefined, 7)).toEqual({ unread: 7, newArrival: false });
		expect(attentionArgs(0, 1)).toEqual({ unread: 1, newArrival: true });
		expect(attentionArgs(2, 1)).toEqual({ unread: 1, newArrival: false });
		expect(attentionArgs(5, 5)).toEqual({ unread: 5, newArrival: false });
	});
});
