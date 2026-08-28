// QURATOR-138 — regression suite for the publish-by-default profile persistence
// (src/lib/profile-autopublish.ts). CLAUDE.md §5 applies: this module drives RELAY WRITES.
//
// What each test pins, and the mutation probe that must red it (run against PRODUCTION, one at a
// time, revert, re-run):
//   coalescing  — delete the `if (timer) clearTimeout(timer); timer = setTimeout(...)` re-arm in
//                 edit() (i.e. fire publishNow() directly) → N-edit tests RED with N publishes.
//   flush       — delete the publishNow() call inside destroy() → flush-on-navigate test RED.
//   failure     — make publishNow() swallow the catch (return instead of onError) → failure test RED.
// A debounce that only ever ran under fake timers is the classic vacuous control, so the last
// describe block runs REAL 40ms timers (no fakes) through the page-exported test hook.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
	createAutopublish,
	setAutopublishDebounceForTests,
	AUTOPUBLISH_DEBOUNCE_MS,
} from './profile-autopublish.js';
import type { Profile } from './types.js';

const PROF: Profile = {
	display_name: 'Auto',
	bio: undefined,
	tags: [],
	since: undefined,
	est_size: undefined,
	languages: [],
	contact_hint: undefined,
	email: undefined,
	location: undefined,
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-27T00:00:00Z',
};

function makeDeps() {
	return {
		save: vi.fn().mockResolvedValue(undefined),
		publish: vi.fn().mockResolvedValue(undefined),
		onError: vi.fn(),
		neverPublished: () => false,
	};
}

describe('profile-autopublish (fake timers)', () => {
	beforeEach(() => { vi.useFakeTimers(); });
	afterEach(() => { vi.useRealTimers(); });

	it('persists locally IMMEDIATELY — no debounce on the local save', () => {
		const deps = makeDeps();
		const form = { ...PROF };
		const c = createAutopublish(() => form, deps);
		c.edit();
		expect(deps.save).toHaveBeenCalledTimes(1); // before any timer advances
		expect(deps.publish).not.toHaveBeenCalled();
		c.cancel();
	});

	it('publishes once after the debounce window, not per edit (coalescing)', async () => {
		const deps = makeDeps();
		const form = { ...PROF };
		const c = createAutopublish(() => form, deps);
		c.edit();
		form.display_name = 'a1';
		c.edit();
		form.display_name = 'a2';
		c.edit();
		await vi.advanceTimersByTimeAsync(AUTOPUBLISH_DEBOUNCE_MS + 10);
		expect(deps.publish).toHaveBeenCalledTimes(1);
		expect(deps.save).toHaveBeenCalledTimes(4); // 3 immediate local saves + 1 re-save at publish
		c.cancel();
	});

	it('a SECOND burst after the window publishes again (debounce, not one-shot)', async () => {
		const deps = makeDeps();
		const form = { ...PROF };
		const c = createAutopublish(() => form, deps);
		c.edit();
		await vi.advanceTimersByTimeAsync(AUTOPUBLISH_DEBOUNCE_MS + 10);
		c.edit();
		await vi.advanceTimersByTimeAsync(AUTOPUBLISH_DEBOUNCE_MS + 10);
		expect(deps.publish).toHaveBeenCalledTimes(2);
		c.cancel();
	});

	it('flush() publishes immediately and disarms the timer (no double publish)', async () => {
		const deps = makeDeps();
		const c = createAutopublish(() => ({ ...PROF }), deps);
		c.edit();
		await c.flush();
		expect(deps.publish).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(AUTOPUBLISH_DEBOUNCE_MS + 10);
		expect(deps.publish).toHaveBeenCalledTimes(1); // armed timer was consumed by the flush
	});

	it('destroy() flushes a pending burst — navigate-away does not drop the last edit', async () => {
		const deps = makeDeps();
		const c = createAutopublish(() => ({ ...PROF }), deps);
		c.edit();
		// NOT a single timer tick — the user typed and immediately closed the page.
		await c.destroy();
		expect(deps.publish).toHaveBeenCalledTimes(1);
		expect(deps.save).toHaveBeenCalled();
	});

	it('a publish failure is SURFACED and the local edit stays saved', async () => {
		const deps = makeDeps();
		deps.publish.mockRejectedValue(new Error('relay unreachable'));
		const c = createAutopublish(() => ({ ...PROF }), deps);
		c.edit();
		await c.flush();
		expect(deps.publish).toHaveBeenCalledTimes(1);
		expect(deps.onError).toHaveBeenCalledTimes(1);
		expect(deps.onError.mock.calls[0][0]).toMatch(/relay unreachable/);
		// The edit is NOT lost: the local save ran both immediately and in the publish path.
		expect(deps.save.mock.calls.length).toBeGreaterThanOrEqual(2);
	});

	it('a publish failure is NOT rendered as success — onError, never a silent resolve', async () => {
		const deps = makeDeps();
		deps.publish.mockRejectedValue(new Error('nope'));
		const c = createAutopublish(() => ({ ...PROF }), deps);
		await c.flush();
		// A resolved promise with no error surface would be the QURATOR-83/134/135 class: unknown
		// rendered as a confident outcome.
		expect(deps.onError).toHaveBeenCalled();
		const msg = deps.onError.mock.calls[0][0];
		expect(msg).not.toMatch(/published|success|saved\.$/i);
	});

	it('a failing LOCAL save is surfaced too (the edit must not exist only in RAM)', async () => {
		const deps = makeDeps();
		deps.save.mockRejectedValue(new Error('disk full'));
		const c = createAutopublish(() => ({ ...PROF }), deps);
		c.edit();
		await vi.advanceTimersByTimeAsync(0); // let the void'd persistLocal() reject
		expect(deps.onError).toHaveBeenCalledTimes(1);
		c.cancel();
	});

	it('a failed publish retries on the next burst (failure is not sticky)', async () => {
		const deps = makeDeps();
		deps.publish.mockRejectedValueOnce(new Error('flaky'));
		const c = createAutopublish(() => ({ ...PROF }), deps);
		c.edit();
		await c.flush();
		expect(deps.publish).toHaveBeenCalledTimes(1);
		c.edit();
		await c.flush();
		expect(deps.publish).toHaveBeenCalledTimes(2); // the retry went out
		expect(deps.onError).toHaveBeenCalledTimes(1); // and only one failure was surfaced
	});

	it('destroy() after destroy() does not double-publish', async () => {
		const deps = makeDeps();
		const c = createAutopublish(() => ({ ...PROF }), deps);
		c.edit();
		await c.destroy();
		await c.destroy();
		expect(deps.publish).toHaveBeenCalledTimes(1);
	});

	it('an in-flight publish is not re-entered by a flush', async () => {
		const deps = makeDeps();
		let resolvePub!: () => void;
		deps.publish.mockImplementation(() => new Promise<void>(r => { resolvePub = r; }));
		const c = createAutopublish(() => ({ ...PROF }), deps);
		c.edit();
		const flushing = c.flush();
		await c.flush(); // second flush while the first publish is still in flight
		resolvePub();
		await flushing;
		expect(deps.publish).toHaveBeenCalledTimes(1);
	});
});

describe('profile-autopublish (REAL timers — not the vacuous fake-only control)', () => {
	// The ticket/CLAUDE.md warning, verbatim in spirit: a debounce that only ever ran under fake
	// timers is the classic vacuous control. This block runs the same controller with NO timer
	// fakes and a 40ms window via the exported test hook.
	beforeEach(() => { setAutopublishDebounceForTests(40); });
	afterEach(() => { vi.restoreAllMocks(); });

	const wait = (ms: number) => new Promise(r => setTimeout(r, ms));

	it('N rapid real-time edits coalesce into ONE publish', async () => {
		const deps = makeDeps();
		const form = { ...PROF };
		const c = createAutopublish(() => form, deps);
		for (let i = 0; i < 5; i++) {
			form.display_name = `n${i}`;
			c.edit();
			await wait(5);
		}
		await wait(80);
		expect(deps.publish).toHaveBeenCalledTimes(1);
		expect(deps.save).toHaveBeenCalledTimes(6); // 5 immediate + 1 publish-time re-save
		await c.destroy();
	});

	it('a real-timer publish failure surfaces the error', async () => {
		const deps = makeDeps();
		deps.publish.mockRejectedValue(new Error('real-timer failure'));
		const c = createAutopublish(() => ({ ...PROF }), deps);
		c.edit();
		await wait(80);
		expect(deps.publish).toHaveBeenCalledTimes(1);
		expect(deps.onError).toHaveBeenCalledTimes(1);
		expect(deps.onError.mock.calls[0][0]).toMatch(/real-timer failure/);
	});
});
