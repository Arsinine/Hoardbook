// @vitest-environment jsdom
// Owner feedback #3 + #4 — the drop-to-create zone at the bottom of the group list.
//
// #3: "Instead of manually creating 'New group' there should a section in the bottom screen where
//     if you drag a card there it automatically creates a new group."
// #4: "There should be copy in Contact Groups that you can drag these cards."
// The approved design answers both with ONE element: the zone's resting copy IS the #4 copy.
//
// This is a BEHAVIOURAL mount test (contacts-drag-protected-mode.test.ts is the mount idiom used
// in this directory): it mounts Contacts, lifts a real row with dragstart, fires dragover/drop at
// the zone, and asserts the popover the card-on-card gesture already opens.
//
// ⚠ P-13 — jsdom computes no layout. These tests CANNOT prove the zone's position at the bottom of
// the list, nor that the resting/armed/hovered CSS renders (dashed border, accent fill). They pin
// the copy, the claim (defaultPrevented), the popover open, and the no-write-on-cancel behaviour.
// Position and styling need eyes on the running app.
//
// MUTATION PROBE (CLAUDE.md §9): neuter onZoneDrop's body in contacts/+page.svelte (make it return
// immediately after preventDefault) and re-run — "a drop on the zone opens the naming popover with
// the dragged contact as sole member" must RED.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ContactsPage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import { DRAG_MIME, dropzoneHoverCopy } from '$lib/drag-group.js';
import type { ContactSummary, Profile } from '$lib/types.js';

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

import { groupsGet, groupsCreateWithMembers } from '$lib/api.js';
const groupsGetMock = groupsGet as unknown as ReturnType<typeof vi.fn>;
const createMock = groupsCreateWithMembers as unknown as ReturnType<typeof vi.fn>;

// ── DataTransfer doubles (jsdom implements neither DragEvent nor DataTransfer) ───────────────

/** Read/write, as during dragstart and drop. */
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

/** Protected mode, as the spec requires during dragover: getData() is blanked. The zone must NOT
 *  read its payload from here — this is the shape that broke the group drop (devtest 2026-08-26). */
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

const ALPHA: ContactSummary = {
	has_browse_key: false,
	npub: 'npub1alpha' + 'a'.repeat(52),
	collections: [],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: PROF('Alpha Hoarder'),
};

const BRAVO: ContactSummary = {
	has_browse_key: false,
	npub: 'npub1bravo' + 'c'.repeat(52),
	collections: [],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: PROF('Bravo Hoarder'),
};

/** Mount with two contacts, lift ALPHA's row, and return the zone. The shared preamble of every
 *  drag test here. */
async function mountWithLiftedCard() {
	groupsGetMock.mockResolvedValue([]);
	contacts.set([ALPHA, BRAVO]);
	const mounted = render(ContactsPage);
	await waitFor(() => expect(groupsGetMock).toHaveBeenCalled());
	await tick();
	const zone = mounted.getByTestId('create-dropzone');
	const row = mounted.getByRole('option', { name: /Alpha Hoarder/ });
	row.dispatchEvent(dragEvent('dragstart', readWriteDT()));
	await tick();
	return { ...mounted, zone, row };
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

describe('dropzoneHoverCopy — the pure multi-select copy', () => {
	it('names the count for a multi-card drag, mirroring the .drag-outcome wording', () => {
		expect(dropzoneHoverCopy(2)).toBe('Release to group 2 contacts');
		expect(dropzoneHoverCopy(5)).toBe('Release to group 5 contacts');
	});
});

describe('drop-to-create zone — resting copy (#4)', () => {
	it('renders the instructional copy on load, before any drag', async () => {
		groupsGetMock.mockResolvedValue([]);
		contacts.set([ALPHA, BRAVO]);
		const { getByText } = render(ContactsPage);
		await waitFor(() => expect(groupsGetMock).toHaveBeenCalled());
		await tick();
		// This IS the #4 acceptance criterion: the "you can drag these cards" copy, always visible.
		expect(getByText('Drag a contact here to start a group')).toBeTruthy();
	});
});

describe('drop-to-create zone — single-card drag (#3)', () => {
	it('a drag over the zone is claimed with the hovered copy naming the contact', async () => {
		const { zone } = await mountWithLiftedCard();
		const over = dragEvent('dragover', protectedModeDT([DRAG_MIME]));
		zone.dispatchEvent(over);
		await tick();
		expect(over.defaultPrevented).toBe(true);
		expect(zone.textContent).toContain('Release to put Alpha Hoarder in a new group');
	});

	it('a drop on the zone opens the naming popover with the dragged contact as sole member', async () => {
		const { zone } = await mountWithLiftedCard();
		zone.dispatchEvent(dragEvent('dragover', protectedModeDT([DRAG_MIME])));
		await tick();
		zone.dispatchEvent(dragEvent('drop', readWriteDT()));
		await tick();
		await waitFor(() => {
			// The SAME namer the card-on-card gesture opens — not a second one.
			const input = document.querySelector('input.dg-input') as HTMLInputElement | null;
			expect(input).not.toBeNull();
			expect((input as HTMLInputElement).placeholder).toBe('Name this group');
		});
		// One-member sub-line (only a zone drop can produce a length-1 array).
		const subline = document.querySelector('.dg-subline');
		expect(subline?.textContent).toBe('Alpha Hoarder will be the first member.');
		// Nothing is written until the name is confirmed — the popover opens with no create call.
		expect(createMock).not.toHaveBeenCalled();
	});

	it('Escape in the popover cancels — no group is created', async () => {
		const { zone } = await mountWithLiftedCard();
		zone.dispatchEvent(dragEvent('dragover', protectedModeDT([DRAG_MIME])));
		await tick();
		zone.dispatchEvent(dragEvent('drop', readWriteDT()));
		await tick();
		const input = await waitFor(() => {
			const el = document.querySelector('input.dg-input') as HTMLInputElement | null;
			expect(el).not.toBeNull();
			return el as HTMLInputElement;
		});
		await fireEvent.keyDown(input, { key: 'Escape' });
		await tick();
		expect(document.querySelector('input.dg-input')).toBeNull();
		expect(createMock).not.toHaveBeenCalled();
	});
});

describe('drop-to-create zone — multi-select drag', () => {
	it('a two-card drag hovers with the count copy and drops as ONE group', async () => {
		groupsGetMock.mockResolvedValue([]);
		contacts.set([ALPHA, BRAVO]);
		const mounted = render(ContactsPage);
		await waitFor(() => expect(groupsGetMock).toHaveBeenCalled());
		await tick();
		const { getByRole, getByTestId } = mounted;
		const zone = getByTestId('create-dropzone');
		const alpha = getByRole('option', { name: /Alpha Hoarder/ });
		const bravo = getByRole('option', { name: /Bravo Hoarder/ });

		// Build the selection the way the page does: plain mousedown anchors, ctrl-click extends.
		await fireEvent.mouseDown(alpha);
		await fireEvent.mouseDown(bravo, { ctrlKey: true });
		const startDT = readWriteDT();
		alpha.dispatchEvent(dragEvent('dragstart', startDT));
		await tick();

		zone.dispatchEvent(dragEvent('dragover', protectedModeDT([DRAG_MIME])));
		await tick();
		expect(zone.textContent).toContain('Release to group 2 contacts');

		zone.dispatchEvent(dragEvent('drop', readWriteDT()));
		await tick();
		await waitFor(() => expect(document.querySelector('input.dg-input')).not.toBeNull());
		// No one-member sub-line on the multi path.
		expect(document.querySelector('.dg-subline')).toBeNull();
		expect(createMock).not.toHaveBeenCalled();
	});
});
