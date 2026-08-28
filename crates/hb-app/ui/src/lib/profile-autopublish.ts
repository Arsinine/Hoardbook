// QURATOR-138 — publish-by-default profile persistence.
//
// The owner's ask: delete the "Save draft" buttons; "any and all changes are pushed
// automatically." The shape (from the ticket, not invented here):
//   1. persist LOCALLY immediately — `saveProfile(form)` runs as soon as an edit lands, so a
//      crash/offline window never loses the edit even if the relay write fails;
//   2. PUBLISH on a debounce (~1-2s idle) — a relay write per keystroke is not acceptable;
//   3. flush on blur and on navigate-away (component destroy) — typing then immediately leaving
//      must not drop the last edit;
//   4. N rapid edits coalesce into ONE publish;
//   5. a publish failure leaves the local edit intact and is SURFACED — never a silent loss, and
//      never an unknown/failed state rendered as success (QURATOR-83/134/135 bug class).
//
// Relay citizenship: the publish goes through the same `publishProfile()` Tauri command the
// explicit button used, and that command's relay write runs through hb-net's shared client, whose
// `RelayRateLimiter` (`relay_writes()` governor) paces writes to stay under relay limits. The
// debounce here is a second guard on top, keeping the call COUNT down (N edits → 1 publish) — it
// does not replace the backend limiter.

import type { Profile } from './types.js';

/** Debounce window in ms. ~1-2s idle per the ticket; configurable so tests can run a real (not
 *  faked) 40ms timer — a debounce that only ever ran under fake timers is the classic vacuous
 *  control (CLAUDE.md §9 / ticket warning). */
export const AUTOPUBLISH_DEBOUNCE_MS = 1200;
export const AUTOPUBLISH_TEST_DEBOUNCE_MS = 40;

let debounceMs = AUTOPUBLISH_DEBOUNCE_MS;
export function setAutopublishDebounceForTests(ms: number) {
	debounceMs = ms;
}
export function resetAutopublishDebounce() {
	debounceMs = AUTOPUBLISH_DEBOUNCE_MS;
}

export interface AutopublishDeps {
	/** Persist the draft locally. Called once per edit burst, immediately (no debounce) so the
	 *  edit survives even if the publish never fires. */
	save: (profile: Profile) => Promise<void>;
	/** Write the published profile to relays. Called at most once per edit burst. */
	publish: () => Promise<void>;
	/** True while a first publish has not yet succeeded (mirrors `publishedSnapshot === null`) —
	 *  used only for the surfaced failure message, not to gate anything. */
	neverPublished?: () => boolean;
	/** Surface a failure. The default is a no-op so a DI-less caller (tests) must opt in; the page
	 *  wires this to the app's `toast(..., 'error')`. */
	onError?: (message: string) => void;
	/** Called after a successful publish (NOT after a local-only save) — the page uses it to
	 *  refresh its as-published snapshot so the status line doesn't keep reading "Unpublished
	 *  changes" (the QURATOR-95 race class). */
	onPublished?: () => void;
}

export interface AutopublishController {
	/** An edit happened (or may have): persist locally NOW, (re)arm the publish debounce. */
	edit: () => void;
	/** Flush a pending burst: persist + publish immediately, clearing any armed timer. */
	flush: () => Promise<void>;
	/** Drop any armed timer WITHOUT publishing (used when the edit turned out to be a no-op,
	 *  e.g. the form was re-seeded from the store). */
	cancel: () => void;
	/** True while a publish is in flight (for a "Publishing…" affordance). */
	isPublishing: () => boolean;
	/** Component teardown: flush (navigate-away must not drop the last edit), then disarm. */
	destroy: () => Promise<void>;
}

/** The `form` handed in must be a live reference the caller keeps mutating (a Svelte-5 `$state`
 *  proxy): the debounce fires against whatever the profile is at flush time, so coalescing is
 *  inherent — N edits within the window read the form once and publish once. */
export function createAutopublish(form: () => Profile, deps: AutopublishDeps): AutopublishController {
	let timer: ReturnType<typeof setTimeout> | undefined;
	let publishing = false;
	let destroyed = false;

	async function persistLocal() {
		try {
			await deps.save(form());
		} catch (e) {
			// The local save is the safety net for the edit; if even this fails the user must hear
			// about it — a silent failure here means the edit exists only in RAM.
			deps.onError?.(String(e));
		}
	}

	async function publishNow() {
		if (publishing || destroyed) return;
		publishing = true;
		try {
			await deps.save(form()); // re-save: the form may have moved on since the local save
			await deps.publish();
			deps.onPublished?.();
		} catch (e) {
			// The edit itself is intact: it was persisted locally above, and `form` still holds it.
			// Surface, never swallow — and never render this as success (QURATOR-83/134/135 class).
			deps.onError?.(`Couldn't publish your profile changes: ${String(e)}. They're saved locally and will go out with your next edit.`);
		} finally {
			publishing = false;
		}
	}

	return {
		edit() {
			if (destroyed) return;
			void persistLocal();
			if (timer) clearTimeout(timer);
			timer = setTimeout(() => {
				timer = undefined;
				void publishNow();
			}, debounceMs);
		},
		async flush() {
			if (timer) { clearTimeout(timer); timer = undefined; }
			await publishNow();
		},
		cancel() {
			if (timer) { clearTimeout(timer); timer = undefined; }
		},
		isPublishing: () => publishing,
		async destroy() {
			if (destroyed) return;
			if (timer) { clearTimeout(timer); timer = undefined; }
			// Flush FIRST, then latch `destroyed` — the guard must not eat the navigate-away flush.
			await publishNow();
			destroyed = true;
		},
	};
}
