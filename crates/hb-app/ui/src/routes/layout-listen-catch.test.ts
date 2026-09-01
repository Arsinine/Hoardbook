// @vitest-environment jsdom
// The `.catch(() => { })` terminators on the layout's two listen() chains (`update-available`,
// `dm-received`) are LOAD-BEARING, not decoration: outside Tauri — every jsdom mount of this layout
// that does not stub the event module — listen() REJECTS, and the rejection then has no handler,
// which vitest counts as a run-level error. On 2026-09-01 exactly this shape produced a run
// reporting 1400/1400 tests PASSED with process exit code 1 (25 unhandled rejections): all tests
// green does not mean the suite passed.
//
// DETECTOR: a process-level 'unhandledRejection' listener, armed before the mount and removed after.
// It must observe the rejection WITHOUT handling it — so the test never awaits (or .catches) any
// promise the mock returns; touching one would mark production's own promise handled and make this
// file decorative. The green path fires zero 'unhandledRejection' events, which is also why the
// precondition asserts are separate: a silently-not-called mock would otherwise read as "clean".
//
// MUTATION PROBE (CLAUDE.md §9 / P-10 — a green test proves nothing until seen red): delete EITHER
// `.catch(() => { })` from the two listen chains in +layout.svelte and
// "both listen() chains absorb their rejection outside Tauri" reds (the captured-rejections array
// grows by one; vitest additionally reports the unhandled error itself). The success-path test is
// the guard's other half: it proves the catch did not break unlisten assignment or teardown.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { readable } from 'svelte/store';
import { render, cleanup } from '@testing-library/svelte';
import { tick } from 'svelte';

vi.mock('$app/stores', () => ({ page: readable({ url: new URL('http://localhost/') }) }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '0.17.0') }));
// The layout mounts WindowControls, whose onMount calls getCurrentWindow() — that dereferences
// window.__TAURI_INTERNALS__.metadata, which jsdom has not got. Stub the module (same note as
// error-toast-sticky.test.ts).
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
	openRepoPage: vi.fn(),
}));

// The module under test's stand-in. In reject mode listen() throws — the real behaviour outside
// Tauri. In resolve mode it hands back a recorded unlisten fn so the teardown contract is pinnable.
const mockMode = vi.hoisted(() => ({ reject: true }));
const listenNames = vi.hoisted(() => new Array<string>());
const offFns = vi.hoisted(() => new Array<() => void>());
vi.mock('@tauri-apps/api/event', () => ({
	listen: vi.fn(async (name: string, _handler: (e: { payload: unknown }) => void) => {
		listenNames.push(name);
		if (mockMode.reject) {
			throw new Error(`listen("${name}") rejects outside Tauri — no __TAURI_INTERNALS__`);
		}
		const off = vi.fn();
		offFns.push(off);
		return off;
	}),
}));

import Layout from './+layout.svelte';

afterEach(() => {
	cleanup();
	listenNames.length = 0;
	offFns.length = 0;
	mockMode.reject = true;
});

describe('the layout listens without leaking unhandled rejections outside Tauri', () => {
	it('both listen() chains absorb their rejection outside Tauri', async () => {
		mockMode.reject = true;
		// Arm the detector and prove it is armed — a listener that never attached would read every
		// run as clean, which is the vacuous-control failure this file exists to prevent.
		const captured: unknown[] = [];
		const onUnhandled = (reason: unknown) => { captured.push(reason); };
		const baseline = process.listenerCount('unhandledRejection');
		process.on('unhandledRejection', onUnhandled);
		expect(
			process.listenerCount('unhandledRejection'),
			'the unhandledRejection detector did not attach',
		).toBe(baseline + 1);

		try {
			render(Layout);
			await tick();
			// unhandledRejection fires on a later macrotask than the rejection itself; give it one.
			await new Promise((r) => setTimeout(r, 20));

			// PRECONDITION, loud: the mount must actually have driven the mocked module through both
			// registrations — otherwise the empty array below asserts nothing. No silent bail, ever.
			expect(
				listenNames,
				'the layout did not register both listeners through the rejecting mock',
			).toEqual(['update-available', 'dm-received']);

			// THE PIN: outside Tauri both listen() promises reject, and neither rejection may escape.
			expect(
				captured,
				'an unhandled rejection escaped the layout (missing .catch on a listen chain) — ' +
				'this fails the whole vitest run even with every test green',
			).toEqual([]);
		} finally {
			process.off('unhandledRejection', onUnhandled);
		}
	});

	it('inside Tauri the unlisten fns are still assigned and torn down (the catch changed nothing)', async () => {
		mockMode.reject = false;
		render(Layout);
		await tick();

		// Loud precondition: the resolving mock must have produced both unlisten fns.
		expect(offFns, 'the resolving listen mock was never called').toHaveLength(2);
		expect(listenNames).toEqual(['update-available', 'dm-received']);

		cleanup();
		expect(offFns[0], 'unlistenUpdate was not called on unmount').toHaveBeenCalled();
		expect(offFns[1], 'unlistenDm was not called on unmount').toHaveBeenCalled();
	});
});
