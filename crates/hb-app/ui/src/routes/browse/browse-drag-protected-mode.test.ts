// @vitest-environment jsdom
// Devtest 2026-08-26 item 2, Browse half. Contacts and Browse are the standing drift pair for
// drag-to-group (CLAUDE.md §9: "one call site gets the fix, its twin doesn't"), so the same
// protected-mode defect and the same behavioural proof exist on both pages.
//
// See contacts/contacts-drag-protected-mode.test.ts for the full explanation of protected mode.
// Short version: during `dragover` the DataTransfer blanks `getData()`, so `onGroupDragOver`
// reading the payload there never reached `preventDefault()` and every drop was refused.
//
// MUTATION PROBE: restore the old head of `onGroupDragOver` in browse/+page.svelte —
//     const sourceNpub = readDragPayload(e.dataTransfer);
//     if (!sourceNpub) return;
// — and re-run this file. "dragover over a group section head is claimed" must RED.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import { DRAG_MIME } from '$lib/drag-group.js';
import type { CachedPeer, Profile } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	groupsGet: vi.fn(),
	groupsCreate: vi.fn(),
	groupsCreateWithMembers: vi.fn(),
	groupsAssign: vi.fn(),
	groupsDelete: vi.fn(),
	groupsUnassign: vi.fn(),
	contactUpdateGroups: vi.fn(),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse') }) };
});
vi.mock('$app/stores', () => stubPage);

import { groupsGet } from '$lib/api.js';
const groupsGetMock = groupsGet as unknown as ReturnType<typeof vi.fn>;

/** A DataTransfer as it behaves during `dragstart`: reads and writes both work. */
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

/** A DataTransfer in the spec's protected mode: `types` readable, `getData()` blanked. */
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

describe('Browse — a drag onto a group is claimed during dragover (devtest item 2)', () => {
	it('dragover over a group section head is claimed (preventDefault called)', async () => {
		groupsGetMock.mockResolvedValue([{ name: 'Film', pubkeys: [BRAVO.npub] }]);
		contacts.set([ALPHA, BRAVO]);

		const { getByRole } = render(BrowsePage);
		await waitFor(() => expect(groupsGetMock).toHaveBeenCalled());
		await tick();

		const row = getByRole('button', { name: /Alpha Hoarder/ });
		const startDT = readWriteDT();
		row.dispatchEvent(dragEvent('dragstart', startDT));
		await tick();
		expect(startDT.types).toContain(DRAG_MIME);

		const target = getByRole('group', { name: 'Film' });
		const overEvent = dragEvent('dragover', protectedModeDT([DRAG_MIME]));
		target.dispatchEvent(overEvent);
		await tick();

		expect(overEvent.defaultPrevented).toBe(true);
	});

	it('a foreign drag over the same target is NOT claimed', async () => {
		groupsGetMock.mockResolvedValue([{ name: 'Film', pubkeys: [BRAVO.npub] }]);
		contacts.set([ALPHA, BRAVO]);

		const { getByRole } = render(BrowsePage);
		await waitFor(() => expect(groupsGetMock).toHaveBeenCalled());
		await tick();

		const target = getByRole('group', { name: 'Film' });
		const overEvent = dragEvent('dragover', protectedModeDT(['Files']));
		target.dispatchEvent(overEvent);
		await tick();

		expect(overEvent.defaultPrevented).toBe(false);
	});
});
