// @vitest-environment jsdom
// Devtest 2026-08-26 item 2 — "contacts grouping drag is broken, i get a red stop symbol when i try
// dragging a card."
//
// TWO independent defects produced that one symptom. This file pins the webview half; the Tauri half
// is pinned by the tauri.conf.json assertion at the bottom.
//
// The webview half: `onGroupDragOver` decided whether to claim the drop by READING the drag payload
// (`readDragPayload(e.dataTransfer)`). During `dragenter`/`dragover` the DataTransfer is in the HTML
// spec's **protected mode**, where `getData()` returns "" for every type — only `.types` is readable.
// So the read always came back empty, the handler returned before `preventDefault()`, the browser
// refused the drop, and the cursor showed the no-drop symbol for the whole gesture. Row→row dragging
// hid the bug because `onDragOver` already gated on component state, not on the payload.
//
// This is a BEHAVIOURAL mount test, not a source-scan: it mounts Contacts, starts a real drag from
// one row, and fires a `dragover` carrying a protected-mode DataTransfer at a real group drop target.
// The assertion is `defaultPrevented` — the exact bit the browser reads to decide "is this a valid
// drop target", i.e. the exact bit that draws the red stop symbol.
//
// MUTATION PROBE (per CLAUDE.md §9 — a green test proves nothing until seen red): restore the old
// head of `onGroupDragOver` in contacts/+page.svelte —
//     const sourceNpub = readDragPayload(e.dataTransfer);
//     if (!sourceNpub) return;
// — and re-run this file. "dragover over a group section head is claimed" must RED.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { tick } from 'svelte';
import ContactsPage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import { DRAG_MIME, isOurDrag } from '$lib/drag-group.js';
import type { CachedPeer, Profile } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	follow: vi.fn().mockResolvedValue(undefined),
	refreshContact: vi.fn().mockResolvedValue(undefined),
	unfollowContact: vi.fn().mockResolvedValue(undefined),
	setContactTags: vi.fn().mockResolvedValue(undefined),
	groupsGet: vi.fn(),
	groupsCreate: vi.fn().mockResolvedValue(undefined),
	groupsDelete: vi.fn().mockResolvedValue(undefined),
	groupsAssign: vi.fn().mockResolvedValue(undefined),
	groupsUnassign: vi.fn().mockResolvedValue(undefined),
	groupsCreateWithMembers: vi.fn().mockResolvedValue(undefined),
	contactUpdateGroups: vi.fn().mockResolvedValue(undefined),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
	onlineCount: vi.fn().mockResolvedValue({ online: 0, fetched_at: null, relay_set: [] }),
	relayStatus: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	privateAudienceList: vi.fn().mockResolvedValue([]),
	privateAudienceSet: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import { groupsGet } from '$lib/api.js';
const groupsGetMock = groupsGet as unknown as ReturnType<typeof vi.fn>;

// ── DataTransfer doubles ────────────────────────────────────────────────────────────────────
// jsdom implements neither DragEvent nor DataTransfer, so both are modelled here.

/** A DataTransfer as it behaves during `dragstart` and `drop`: reads and writes both work. */
function readWriteDT() {
	const store = new Map<string, string>();
	return {
		types: [] as string[],
		dropEffect: 'none',
		effectAllowed: 'none',
		setData(type: string, value: string) {
			if (!store.has(type)) this.types.push(type);
			store.set(type, value);
		},
		getData(type: string) {
			return store.get(type) ?? '';
		},
	};
}

/** A DataTransfer as the spec requires it to behave during `dragenter`/`dragover`: `types` is
 *  readable, `getData()` is blanked. This is the shape that broke the drop. */
function protectedModeDT(types: string[]) {
	return {
		types,
		dropEffect: 'none',
		effectAllowed: 'copy',
		setData() {},
		getData() {
			return '';
		},
	};
}

/** jsdom has no DragEvent; build a cancellable bubbling Event carrying a dataTransfer. */
function dragEvent(type: string, dataTransfer: unknown) {
	const e = new Event(type, { bubbles: true, cancelable: true });
	Object.defineProperty(e, 'dataTransfer', { value: dataTransfer });
	return e;
}

const PROF = (name: string): Profile => ({
	display_name: name,
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
});

const ALPHA: CachedPeer = {
	npub: 'npub1alpha' + 'a'.repeat(52),
	collections: [],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: PROF('Alpha Hoarder'),
};

const BRAVO: CachedPeer = {
	npub: 'npub1bravo' + 'c'.repeat(52),
	collections: [],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: PROF('Bravo Hoarder'),
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

describe('isOurDrag — the protected-mode-safe claim gate', () => {
	it('recognises our drag from `types` alone, with getData() blanked', () => {
		expect(isOurDrag(protectedModeDT([DRAG_MIME]) as unknown as DataTransfer)).toBe(true);
	});

	it('does NOT claim a foreign drag (a file drag from the OS)', () => {
		expect(isOurDrag(protectedModeDT(['Files', 'text/plain']) as unknown as DataTransfer)).toBe(false);
	});

	it('does not throw on a null dataTransfer', () => {
		expect(isOurDrag(null)).toBe(false);
	});
});

describe('Contacts — a drag onto a group is claimed during dragover (devtest item 2)', () => {
	it('dragover over a group section head is claimed (preventDefault called)', async () => {
		groupsGetMock.mockResolvedValue([{ name: 'Film', pubkeys: [BRAVO.npub] }]);
		contacts.set([ALPHA, BRAVO]);

		const { getByRole } = render(ContactsPage);
		await waitFor(() => expect(groupsGetMock).toHaveBeenCalled());
		await tick();

		// The group drop targets only carry role="group" in the Groups view.
		await fireEvent.click(getByRole('button', { name: 'Groups' }));
		await tick();

		// Lift Alpha's row — this is what captures the carried npubs.
		const row = getByRole('option', { name: /Alpha Hoarder/ });
		const startDT = readWriteDT();
		row.dispatchEvent(dragEvent('dragstart', startDT));
		await tick();
		// Sanity: the drag really did write our payload (so the type below isn't fiction).
		expect(startDT.types).toContain(DRAG_MIME);

		// …and drag it over the "Film" group head, with the DataTransfer in protected mode.
		const target = getByRole('group', { name: 'Film' });
		const overEvent = dragEvent('dragover', protectedModeDT([DRAG_MIME]));
		target.dispatchEvent(overEvent);
		await tick();

		// preventDefault() on dragover IS the "this is a drop target" signal. Without it the browser
		// draws the no-drop cursor — the owner's red stop symbol.
		expect(overEvent.defaultPrevented).toBe(true);
	});

	it('a foreign drag over the same target is NOT claimed', async () => {
		groupsGetMock.mockResolvedValue([{ name: 'Film', pubkeys: [BRAVO.npub] }]);
		contacts.set([ALPHA, BRAVO]);

		const { getByRole } = render(ContactsPage);
		await waitFor(() => expect(groupsGetMock).toHaveBeenCalled());
		await tick();
		await fireEvent.click(getByRole('button', { name: 'Groups' }));
		await tick();

		const target = getByRole('group', { name: 'Film' });
		const overEvent = dragEvent('dragover', protectedModeDT(['Files']));
		target.dispatchEvent(overEvent);
		await tick();

		expect(overEvent.defaultPrevented).toBe(false);
	});
});

describe('Tauri must not intercept the webview drag (the other half of devtest item 2)', () => {
	it('the main window sets dragDropEnabled: false', () => {
		// Tauri v2 defaults `dragDropEnabled` to TRUE, which hands the OS drag-drop handler the
		// gesture before the webview ever sees it — the red stop symbol appears for EVERY drag,
		// including row→row, no matter what the page does. Nothing in this app consumes Tauri's
		// native drag-drop events (no onDragDropEvent listener, no DragDrop handler in Rust), so
		// nothing is LOST by turning it off.
		//
		// ⚠ But it is not free either, and this comment used to claim it was (chorus review
		// 2026-08-27, finding 1). The native handler was also doing SUPPRESSION: with it off, a file
		// dragged in from Explorer and dropped anywhere on the window reaches the webview, which
		// takes the browser default and navigates to the file:// URL — a blank SPA until restart.
		// The replacement is the window-level guard pinned by the next test. The two changes are a
		// pair; neither is correct alone.
		// Resolved from cwd, not import.meta.url: under `@vitest-environment jsdom` import.meta.url
		// is an http:// URL and readFileSync rejects it. vitest's root is crates/hb-app/ui.
		const conf = JSON.parse(readFileSync(resolve(process.cwd(), '../tauri.conf.json'), 'utf8'));
		const main = conf.app.windows.find((w: { label: string }) => w.label === 'main');
		expect(main, 'no window labelled "main" in tauri.conf.json').toBeTruthy();
		expect(main.dragDropEnabled).toBe(false);
	});

	it('the layout refuses foreign drags at the window, so a dropped file cannot navigate the webview', () => {
		// The other half of the pair. Source-scanned rather than mounted: +layout.svelte is the shell
		// every route hangs off, and mounting it drags in the whole onMount fan-out (identity,
		// profile, collections, contacts, messages, read state, topic announcements) plus a Tauri
		// event listener — the repo's established idiom for layout-level wiring is a scan
		// (q81-window-chrome.test.ts does the same). What a scan CAN'T prove is that WebView2 honours
		// the preventDefault; that needs a live file-drop on Windows.
		const layout = readFileSync(resolve(process.cwd(), 'src/routes/+layout.svelte'), 'utf8');

		// Both events, or the guard has a hole: dragover-only still lets `drop` navigate, and
		// drop-only never makes the cursor honest.
		const win = layout.match(/<svelte:window\b[^>]*>/);
		expect(win, '+layout.svelte has no <svelte:window> element').not.toBeNull();
		expect(win![0]).toContain('ondragover={guardForeignDrag}');
		expect(win![0]).toContain('ondrop={guardForeignDrag}');

		// And the guard must exempt OUR drags — otherwise it would fight the per-element handlers
		// over dropEffect and re-break the gesture item 2 exists to fix. Slice the function body so
		// a match elsewhere in the file cannot satisfy this (the W4 missing-import lesson).
		const start = layout.indexOf('function guardForeignDrag(');
		expect(start, 'guardForeignDrag is referenced but not defined').toBeGreaterThan(-1);
		const body = layout.slice(start, layout.indexOf('\n\t}', start));
		expect(body).toMatch(/if \(isOurDrag\(e\.dataTransfer\)\) return;/);
		expect(body).toMatch(/e\.preventDefault\(\);/);
		expect(body).toMatch(/dropEffect = 'none'/);
		expect(layout).toMatch(/import \{[^}]*\bisOurDrag\b[^}]*\} from '\$lib\/drag-group\.js';/);
	});
});
