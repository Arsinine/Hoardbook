// @vitest-environment jsdom
/**
 * QURATOR-379 — the toast system, rewritten (replaces devtest 2026-08-26 item 7's
 * "errors are sticky" ruling, which this file used to pin as error-toast-sticky.test.ts).
 *
 * New shape, pinned at the two places production implements it:
 *   1. lib/stores.ts   — errors STACK: one bottom-anchored column, oldest at the bottom, each
 *                        error with its OWN 15s timer (TOAST_ERROR_MS), its own ✕, dedupe by
 *                        text, cap 5 (lane default). Success/Undo keep the single replace-not-
 *                        queue slot (TOAST_MS 7s / TOAST_ACTION_MS 12s) and never touch the stack.
 *   2. +layout.svelte  — .toast-stack renders the single slot FIRST (column-reverse puts it at
 *                        the bottom) then the error stack in arrival order; each error carries
 *                        role="alert" and a ✕ wired to dismissError; the single slot keeps
 *                        role="status" (success) / role="alert" (error kind) + dismissToast.
 *
 * stores.ts is the ONLY writer of toastMessage/errorToasts (verified by rg over src/, excluding
 * tests), so store-level pins cover every error message in the app. The layout half is MOUNTED,
 * not source-scanned — per CLAUDE.md, "we source-scan because it cannot mount" is not a valid
 * justification here.
 *
 * The animate:flip slide-down (200ms, 0 under prefers-reduced-motion) is NOT covered here:
 * jsdom computes no layout, so nothing in vitest can observe a FLIP animation. It is covered
 * visually via the Playwright stub sequence (see the QURATOR-379 report).
 *
 * MUTATION PROBES (per CLAUDE.md §9 — a green test proves nothing until seen red), each applied
 * alone and re-run single-file, then restored by re-applying the inverse edit (never git
 * checkout); each named next to the test it reds:
 *   M1 TOAST_ERROR_MS → 7000                         → the 14999/15000 boundary test reds
 *   M2 per-id Map → one shared module timer          → the per-error deadline test reds (the
 *     first error never expires; only the newest does)
 *   M3 pushError prepends instead of appending       → the arrival-order (DOM) test reds
 *   M4 dedupe branch deleted                         → the duplicate-collapse test reds (two copies)
 *   M5 cap branch deleted                            → the 6th-evicts test reds (6 stay)
 *   M6 toast() success also clears errorToasts       → the success-leaves-the-stack test reds
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

import { toastMessage, errorToasts, toast, toastWithAction, dismissToast, dismissError } from '$lib/stores.js';
import Layout from './+layout.svelte';

// jsdom has no Web Animations API, and `animate:flip` needs it the moment one toast leaves the
// stack while siblings stay (flip re-positions the survivors). A no-op Animation satisfies Svelte;
// the real slide is covered visually via the Playwright stub sequence (see the header note).
if (typeof Element !== 'undefined' && !('animate' in Element.prototype)) {
	Object.defineProperty(Element.prototype, 'animate', {
		value: () => ({ cancel: () => {}, finished: Promise.resolve(), playbackRate: 1, onfinish: null }),
		configurable: true,
		writable: true,
	});
	Object.defineProperty(Element.prototype, 'getAnimations', {
		value: () => [],
		configurable: true,
		writable: true,
	});
}

/** errorToasts is module state — a leak from one test into the next is a false pass, so every
 *  test starts from an empty stack (dismissError is the production removal path). */
function clearErrors() {
	for (const e of get(errorToasts)) dismissError(e.id);
}

describe('toast store — errors stack with 15s per-error timers (QURATOR-379)', () => {
	beforeEach(() => { vi.useFakeTimers(); dismissToast(); clearErrors(); });
	afterEach(() => { clearErrors(); dismissToast(); vi.useRealTimers(); });

	it('an error is visible at 14,999ms and gone at 15,000ms (its own 15s lifetime)', () => {
		// MUTATION M1: set TOAST_ERROR_MS to 7000 → reds here (gone before 14,999).
		toast('This collection’s full list is too large to send over the connection', 'error');
		expect(get(errorToasts).map((e) => e.text)).toEqual([
			'This collection’s full list is too large to send over the connection',
		]);
		vi.advanceTimersByTime(14_999);
		expect(get(errorToasts).length, 'must still be readable at 14,999ms').toBe(1);
		vi.advanceTimersByTime(1);
		expect(get(errorToasts).length, 'must be gone at 15,000ms').toBe(0);
		// MUTATION M2 (second half): delete the setTimeout in scheduleError → the error never
		// expires; the 15,000ms assertion above reds instead.
	});

	it('each error expires at ITS OWN deadline — a newer error never extends an older one', () => {
		// MUTATION M2: replace the per-id errorTimers Map with ONE shared module timer
		// (set/cleared per push) → only the newest error would ever fire; e1/e2 red here.
		toast('first failure', 'error');
		vi.advanceTimersByTime(5_000);
		toast('second failure', 'error');
		vi.advanceTimersByTime(5_000);
		toast('third failure', 'error');
		// t = 10s: e1 is at its deadline now (10s + nothing left of its original 15s… e1 was
		// pushed at t=0, so it expires at t=15s).
		expect(get(errorToasts).map((e) => e.text)).toEqual(['first failure', 'second failure', 'third failure']);
		vi.advanceTimersByTime(5_000); // t = 15s: only e1's deadline
		expect(get(errorToasts).map((e) => e.text)).toEqual(['second failure', 'third failure']);
		vi.advanceTimersByTime(5_000); // t = 20s: only e2's
		expect(get(errorToasts).map((e) => e.text)).toEqual(['third failure']);
		vi.advanceTimersByTime(5_000); // t = 25s: e3's own deadline
		expect(get(errorToasts).length).toBe(0);
	});

	it('the ✕ dismisses early (before the 15s timer fires)', () => {
		toast('boom', 'error');
		const [e] = get(errorToasts);
		dismissError(e.id);
		expect(get(errorToasts).length).toBe(0);
		// A later timer for the dismissed id must not resurrect anything (it can't: the timer
		// was cleared, and removeError filters by id). Advance to be sure nothing throws.
		vi.advanceTimersByTime(20_000);
		expect(get(errorToasts).length).toBe(0);
	});

	it('a repeated error collapses into ONE toast — no second copy, timer restarted', () => {
		// MUTATION M4: delete the dup branch in pushError → reds (two copies on screen).
		toast('relay unreachable', 'error');
		vi.advanceTimersByTime(10_000);
		toast('relay unreachable', 'error'); // same text 10s later
		expect(get(errorToasts).length, 'duplicate text must collapse, not stack').toBe(1);
		// The restart is observable: the single copy now expires at t=25s, not t=15s.
		vi.advanceTimersByTime(5_000); // t = 15s — would have killed the original timer
		expect(get(errorToasts).length, 'timer must have restarted on the repeat').toBe(1);
		vi.advanceTimersByTime(10_000); // t = 25s — the restarted deadline
		expect(get(errorToasts).length).toBe(0);
	});

	it('a 6th error evicts the OLDEST (cap 5; the lane default, not the owner’s number)', () => {
		// MUTATION M5: delete the cap branch in pushError → reds (6 stay).
		for (const t of ['e1', 'e2', 'e3', 'e4', 'e5', 'e6']) toast(t, 'error');
		expect(get(errorToasts).map((e) => e.text)).toEqual(['e2', 'e3', 'e4', 'e5', 'e6']);
	});

	it('a success toast does not touch the error stack (neither clears nor replaces it)', () => {
		// MUTATION M6: make toast() clear errorToasts on the success path → reds here.
		toast('listing too large', 'error');
		toast('Collection added');
		expect(get(errorToasts).map((e) => e.text)).toEqual(['listing too large']);
		expect(get(toastMessage)?.text).toBe('Collection added');
		expect(get(toastMessage)?.kind).toBe('success');
	});

	it('success keeps its 7s single-slot lifetime; success+Undo keeps 12s', () => {
		toast('Collection added');
		vi.advanceTimersByTime(6_999);
		expect(get(toastMessage), 'success must outlast 6,999ms').not.toBeNull();
		vi.advanceTimersByTime(1);
		expect(get(toastMessage)).toBeNull();

		toastWithAction('Moved to Film', { label: 'Undo', run: () => {} });
		vi.advanceTimersByTime(11_999);
		expect(get(toastMessage), 'Undo toast must outlast 11,999ms').not.toBeNull();
		vi.advanceTimersByTime(1);
		expect(get(toastMessage)).toBeNull();
	});

	it('an error-kind action toast stays on the SINGLE slot (no production caller emits one) at 15s', () => {
		toastWithAction('remove failed', { label: 'Undo', run: () => {} }, 'error');
		expect(get(errorToasts).length, 'must NOT enter the error stack').toBe(0);
		vi.advanceTimersByTime(14_999);
		expect(get(toastMessage)).not.toBeNull();
		vi.advanceTimersByTime(1);
		expect(get(toastMessage)).toBeNull();
	});
});

describe('the layout renders the stack (QURATOR-379)', () => {
	beforeEach(() => { dismissToast(); clearErrors(); });
	afterEach(() => { cleanup(); clearErrors(); dismissToast(); });

	async function mountLayout() {
		render(Layout);
		// The toast block is a SIBLING of .frame, outside the appReady gate, so it renders as soon as
		// the store has a value — no need to wait on the layout's async init.
		await waitFor(() => expect(document.body.querySelector('.frame')).toBeTruthy());
	}

	it('three errors stack in ARRIVAL order in the DOM (column-reverse puts the newest on top)', async () => {
		// MUTATION M3: prepend instead of append in pushError → reds (DOM order reverses).
		await mountLayout();
		toast('e1', 'error');
		toast('e2', 'error');
		toast('e3', 'error');
		await waitFor(() => expect(screen.getAllByRole('alert').length).toBe(3));
		const texts = [...document.body.querySelectorAll('.toast-stack .toast .toast-text')].map((n) => n.textContent);
		expect(texts).toEqual(['e1', 'e2', 'e3']);
		// The single slot is NOT in the way: no success toast is mounted at all.
		expect(document.body.querySelector('.toast-stack')!.children.length).toBe(3);
	});

	it('the single slot renders BELOW the error stack (first DOM child of the column)', async () => {
		await mountLayout();
		toast('the failure', 'error');
		toast('Collection added');
		await waitFor(() => expect(screen.getByText('the failure')).toBeTruthy());
		const kids = [...document.body.querySelector('.toast-stack')!.children];
		expect(kids.length).toBe(2);
		expect(kids[0]!.textContent).toContain('Collection added');
		expect(kids[1]!.textContent).toContain('the failure');
	});

	it('each stacked error carries role=alert and its own ✕, and a ✕ removes only its own toast', async () => {
		await mountLayout();
		toast('alpha failed', 'error');
		toast('beta failed', 'error');
		await waitFor(() => expect(screen.getAllByRole('alert').length).toBe(2));
		// role=alert per error; both ✕s present.
		const xs = screen.getAllByRole('button', { name: 'Dismiss' });
		expect(xs.length).toBe(2);
		// Dismiss the SECOND (beta, the newest): only it disappears; alpha survives.
		const betaToast = screen.getByText('beta failed').closest('.toast')!;
		const betaX = betaToast.querySelector('.toast-dismiss')!;
		await fireEvent.click(betaX);
		expect(screen.queryByText('beta failed')).toBeNull();
		expect(screen.getByText('alpha failed')).toBeTruthy();
		expect(screen.getAllByRole('button', { name: 'Dismiss' }).length).toBe(1);
	});

	it('a success toast keeps role=status and renders NO dismiss control', async () => {
		await mountLayout();
		toast('Collection added');
		const el = await screen.findByText('Collection added');
		expect(el.closest('.toast')!.getAttribute('role')).toBe('status');
		expect(screen.queryByRole('button', { name: 'Dismiss' })).toBeNull();
	});

	it('the single-slot ERROR kind keeps its ✕ (wired to dismissToast) and role=alert', async () => {
		await mountLayout();
		toastWithAction('remove failed', { label: 'Undo', run: () => {} }, 'error');
		const x = await screen.findByRole('button', { name: 'Dismiss' });
		await fireEvent.click(x);
		expect(get(toastMessage)).toBeNull();
		expect(get(errorToasts).length).toBe(0);
		await waitFor(() => expect(screen.queryByRole('button', { name: 'Dismiss' })).toBeNull());
	});

	it('nothing renders when both stores are empty', async () => {
		await mountLayout();
		expect(document.body.querySelector('.toast-stack')).toBeNull();
	});
});
