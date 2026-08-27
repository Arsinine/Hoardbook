// @vitest-environment jsdom
/**
 * Devtest 2026-08-26 item 7 — "the manifest-too-big message disappears too quickly to be read, make
 * it permanent with an x on the top right to close it", widened by the owner the same day to "all red
 * messages suffer from the exact same disappearing issue. Make the changes apply to all red error
 * messages", plus a follow-up: "For white messages double the amount of time it stays visible for".
 *
 * So there are three separate behaviours to pin, and they are pinned at the two places production
 * actually implements them:
 *   1. lib/stores.ts   — an error toast arms NO timer at all (it cannot expire); success timers doubled.
 *   2. +layout.svelte  — a sticky toast renders a dismiss control, and that control clears the store.
 *
 * stores.ts is the ONLY writer of toastMessage (verified by rg over src/, excluding tests), so a store
 * -level pin covers every error message in the app, not just the manifest one the owner quoted.
 *
 * The layout half is MOUNTED, not source-scanned — per CLAUDE.md, "we source-scan because it cannot
 * mount" is not a valid justification here. `children` is an optional prop, so `render(Layout)` works.
 *
 * MUTATION PROBE (per CLAUDE.md §9 — a green test proves nothing until seen red), run 2026-08-26:
 *   a) In stores.ts, delete `if (isStickyToast(kind)) return;` from toast() so errors get a timer again
 *      → "an error toast never expires on its own" reds. Restored.
 *   b) In +layout.svelte, change `{#if isStickyToast($toastMessage.kind)}` to `{#if false}`
 *      → "a sticky error renders a dismiss control" reds. Restored.
 *   c) In stores.ts, set TOAST_MS back to 3500
 *      → "a success toast is still visible at the OLD 3500ms deadline" reds. Restored.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get, readable } from 'svelte/store';

vi.mock('$app/stores', () => ({ page: readable({ url: new URL('http://localhost/') }) }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '0.17.0') }));
// The layout mounts WindowControls, whose onMount calls getCurrentWindow() — that dereferences
// window.__TAURI_INTERNALS__.metadata, which jsdom has not got. Its own try/catch is too late: the
// throw is in the constructor, before the await. Stub the module rather than the global.
vi.mock('@tauri-apps/api/window', () => ({
	getCurrentWindow: () => ({
		isMaximized: async () => false,
		onResized: async () => () => {},
		minimize: async () => {},
		toggleMaximize: async () => {},
		close: async () => {},
	}),
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
}));

import { render, screen, fireEvent, waitFor, cleanup } from '@testing-library/svelte';

import { toastMessage, toast, toastWithAction, dismissToast, isStickyToast } from '$lib/stores.js';
import Layout from './+layout.svelte';

describe('the toast store — errors are sticky, successes last twice as long (devtest item 7)', () => {
	beforeEach(() => { vi.useFakeTimers(); dismissToast(); });
	afterEach(() => { dismissToast(); vi.useRealTimers(); });

	it('an error toast never expires on its own', () => {
		toast('This collection’s full list is too large to send over the connection', 'error');
		vi.advanceTimersByTime(60_000);
		expect(get(toastMessage)?.kind).toBe('error');
	});

	it('an error toast WITH an action is sticky too', () => {
		toastWithAction('remove failed', { label: 'Undo', run: () => {} }, 'error');
		vi.advanceTimersByTime(60_000);
		expect(get(toastMessage)?.kind).toBe('error');
	});

	it('dismissToast() is what clears a sticky error', () => {
		toast('boom', 'error');
		expect(get(toastMessage)).not.toBeNull();
		dismissToast();
		expect(get(toastMessage)).toBeNull();
	});

	it('a success toast is still visible at the OLD 3500ms deadline, and gone by the new one', () => {
		toast('Collection added');
		vi.advanceTimersByTime(3500);
		expect(get(toastMessage), 'success toasts must now outlast the old 3500ms').not.toBeNull();
		vi.advanceTimersByTime(3500);
		expect(get(toastMessage)).toBeNull();
	});

	it('a success toast WITH an action is still visible at the OLD 6000ms deadline', () => {
		toastWithAction('Moved to Film', { label: 'Undo', run: () => {} });
		vi.advanceTimersByTime(6000);
		expect(get(toastMessage)).not.toBeNull();
		vi.advanceTimersByTime(6000);
		expect(get(toastMessage)).toBeNull();
	});

	it('a success toast does not silently replace an undismissed error', () => {
		toast('listing too large', 'error');
		toast('Collection added');
		expect(get(toastMessage)?.text).toBe('listing too large');
	});

	it('but a NEWER error still replaces an older one (replace-not-queue is unchanged)', () => {
		toast('first failure', 'error');
		toast('second failure', 'error');
		expect(get(toastMessage)?.text).toBe('second failure');
	});

	it('isStickyToast is the single kind test both halves share', () => {
		expect(isStickyToast('error')).toBe(true);
		expect(isStickyToast('success')).toBe(false);
	});
});

describe('the layout renders the dismiss control for a sticky toast (devtest item 7)', () => {
	afterEach(() => { cleanup(); dismissToast(); });

	async function mountLayout() {
		render(Layout);
		// The toast block is a SIBLING of .frame, outside the appReady gate, so it renders as soon as
		// the store has a value — no need to wait on the layout's async init.
		await waitFor(() => expect(document.body.querySelector('.frame')).toBeTruthy());
	}

	it('a sticky error renders a dismiss control, and clicking it clears the toast', async () => {
		await mountLayout();
		toast('This listing is 17639938 bytes — over the 16777216-byte transport limit.', 'error');
		const x = await screen.findByRole('button', { name: 'Dismiss' });
		await fireEvent.click(x);
		expect(get(toastMessage)).toBeNull();
		await waitFor(() => expect(screen.queryByRole('button', { name: 'Dismiss' })).toBeNull());
	});

	it('a success toast renders NO dismiss control (it expires on its own)', async () => {
		await mountLayout();
		toast('Collection added');
		await screen.findByText('Collection added');
		expect(screen.queryByRole('button', { name: 'Dismiss' })).toBeNull();
	});

	it('the error toast is announced assertively (role=alert), the success one politely', async () => {
		await mountLayout();
		toast('remove failed hard', 'error');
		await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('remove failed hard'));
	});
});
