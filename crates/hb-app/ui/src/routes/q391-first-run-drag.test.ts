// @vitest-environment jsdom
// QURATOR-391 devtest item 1 — "When the app first starts, there is no title navbar which means
// the app cannot be drag moved." First run has no identity ⇒ no route .topbar renders, and the
// only drag surface was the layout's 8px `.win-drag-top` strip. Fix: when !$identity the same
// strip carries `win-drag-first-run` and grows to 52px.
//
// MOUNTED (P-4: try mounting first — the layout mounts fine in jsdom, see
// layout-listen-catch.test.ts / q340-unread-attention.test.ts for the recipe).
//
// P-13 LIMIT (stated, as required): jsdom computes no layout and this vitest setup does not
// inject the component's <style>, so the test can prove only that the BAND ELEMENT exists with
// the drag attribute and the first-run class — it CANNOT prove the 52px height or that the band
// misses the onboarding card's controls. Those numbers were measured in a real browser
// (vite dev + Playwright, getBoundingClientRect) and are recorded in the ticket handback.
//
// P-10 MUTATION (seen red): delete `class:win-drag-first-run={!$identity}` from +layout.svelte →
// the first-run test reds (classList lacks win-drag-first-run). Restore → green.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { readable, writable } from 'svelte/store';
import { render, cleanup } from '@testing-library/svelte';
import { tick } from 'svelte';

vi.mock('$app/stores', () => ({ page: readable({ url: new URL('http://localhost/') }) }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '0.27.1') }));
// WindowControls dereferences window.__TAURI_INTERNALS__.metadata via getCurrentWindow() on mount.
vi.mock('@tauri-apps/api/window', () => ({
	getCurrentWindow: () => ({
		isMaximized: async () => false,
		onResized: async () => () => {},
		minimize: async () => {},
		toggleMaximize: async () => {},
		close: async () => {},
	}),
}));
vi.mock('@tauri-apps/api/event', () => ({
	listen: vi.fn(async () => () => {}),
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
	getSettings: vi.fn(async () => ({ theme: 'dark', glass: true })),
}));
// The layout's unread-attention effect calls setUnreadAttention → real module would invoke Tauri.
// Same importOriginal spy pattern as q340-unread-attention.test.ts.
const setUnreadAttention = vi.hoisted(() => vi.fn(async () => {}));
vi.mock('$lib/unread-attention.js', async (importOriginal) => ({
	...(await importOriginal<Record<string, unknown>>()),
	setUnreadAttention,
}));

import Layout from './+layout.svelte';
import { identity } from '$lib/stores.js';

// npub-shaped 63-char string; the layout only slices it (idShort) — nothing validates it here.
const ident = { npub: 'npub1' + 'a'.repeat(58) } as unknown as Parameters<typeof identity.set>[0];

// The layout's onMount IIFE sets the identity store from the (mocked) getIdentity; let those
// microtasks + the store->template propagation settle before asserting, deterministically.
async function flush() {
	await tick();
	await tick();
	await new Promise((r) => setTimeout(r, 0));
	await tick();
}

beforeEach(() => {
	identity.set(null);
});

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
});

describe('QURATOR-391 item 1 — first-run drag band on the layout', () => {
	it('first run (no identity): the drag strip carries the first-run band class and the drag attribute', async () => {
		const { container } = render(Layout);
		await flush();
		const strip = container.querySelector<HTMLElement>('.win-drag-top');
		expect(strip).toBeTruthy();
		expect(strip!.getAttribute('data-tauri-drag-region')).toBe('');
		expect(strip!.classList.contains('win-drag-first-run')).toBe(true);
	});

	it('with an identity: the strip stays the plain 8px fallback (no first-run class), drag attribute intact', async () => {
		identity.set(ident);
		// getIdentity resolves to the same shape, so the mount's own load cannot flip it back.
		vi.mocked((await import('$lib/api.js')).getIdentity).mockResolvedValue(ident);
		const { container } = render(Layout);
		await flush();
		const strip = container.querySelector<HTMLElement>('.win-drag-top');
		expect(strip).toBeTruthy();
		expect(strip!.classList.contains('win-drag-first-run')).toBe(false);
		expect(strip!.getAttribute('data-tauri-drag-region')).toBe('');
	});
});
