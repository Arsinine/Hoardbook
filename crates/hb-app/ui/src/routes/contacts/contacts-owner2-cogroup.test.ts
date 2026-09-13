// @vitest-environment jsdom
// Owner feedback #2 (2026-09-13): "Dragging a card over another card in the same group allows
// you to create another subgroup which we dont want."
//
// BEHAVIOURAL mount tests, not source-scans: the page mounts (render(ContactsPage), mocking only
// $lib/api.js and $app/navigation — the pattern proven by contacts-drag-protected-mode.test.ts),
// a real drag is started from one row, and the refusal is observed where the user sees it:
//
//   1. dragover of a CO-MEMBER row sets dropEffect 'none' — the cursor refuses BEFORE the drop,
//      the same affordance rule the page already states for the W4 group-heading path.
//   2. dropping on a co-member opens NO naming popover — the gesture is refused, not silently
//      re-runnable into a duplicate group.
//   3. control: a NON-co-member target still shows 'copy' on dragover and opens the namer on
//      drop — the valid gesture is unchanged.
//
// MUTATION PROBE (per CLAUDE.md §9 — a green test proves nothing until seen red): make
// alreadyGroupedTogether in src/lib/drag-group.ts return false unconditionally — tests 1 and 2
// must RED. Test 3 must stay green under that same mutation (it pins the allowed case).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ContactsPage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import { DRAG_MIME } from '$lib/drag-group.js';
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

// ── DataTransfer doubles (jsdom implements neither DragEvent nor DataTransfer) ────────────────

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

/** A DataTransfer as the spec requires during `dragenter`/`dragover`: `types` readable,
 *  `getData()` blanked (protected mode). */
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

const CHARLIE: CachedPeer = {
	npub: 'npub1charli' + 'e'.repeat(51),
	collections: [],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: PROF('Charlie Hoarder'),
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

/** Mount the page in the Groups view with Alpha+Bravo co-members of "Film" and Charlie only in
 *  "Music", then lift Alpha's row. Returns the mounted queries and the lifted row. */
async function mountWithAlphaLifted() {
	groupsGetMock.mockResolvedValue([
		{ name: 'Film', pubkeys: [ALPHA.npub, BRAVO.npub] },
		{ name: 'Music', pubkeys: [CHARLIE.npub] },
	]);
	contacts.set([ALPHA, BRAVO, CHARLIE]);

	const tools = render(ContactsPage);
	await waitFor(() => expect(groupsGetMock).toHaveBeenCalled());
	await tick();
	await fireEvent.click(tools.getByRole('button', { name: 'Groups' }));
	await tick();

	const row = tools.getByRole('option', { name: /Alpha Hoarder/ });
	const startDT = readWriteDT();
	row.dispatchEvent(dragEvent('dragstart', startDT));
	await tick();
	expect(startDT.types).toContain(DRAG_MIME); // the drag really carries our payload
	return tools;
}

describe('owner feedback #2 — a co-member drop is refused', () => {
	it('dragover of a CO-MEMBER shows the refuse cursor (dropEffect none) BEFORE the drop', async () => {
		const { getByRole } = await mountWithAlphaLifted();
		const target = getByRole('option', { name: /Bravo Hoarder/ });
		const dt = protectedModeDT([DRAG_MIME]);
		target.dispatchEvent(dragEvent('dragover', dt));
		await tick();
		expect(dt.dropEffect).toBe('none');
	});

	it('dropping on a CO-MEMBER opens no naming popover', async () => {
		const { getByRole, queryByPlaceholderText } = await mountWithAlphaLifted();
		const target = getByRole('option', { name: /Bravo Hoarder/ });
		const dt = readWriteDT();
		dt.setData(DRAG_MIME, ALPHA.npub);
		target.dispatchEvent(dragEvent('drop', dt));
		await tick();
		expect(queryByPlaceholderText('Name this group')).toBeNull();
	});

	it('CONTROL — a non-co-member target still shows copy on dragover and opens the namer on drop', async () => {
		const { getByRole, findByPlaceholderText } = await mountWithAlphaLifted();
		const target = getByRole('option', { name: /Charlie Hoarder/ });

		const overDT = protectedModeDT([DRAG_MIME]);
		target.dispatchEvent(dragEvent('dragover', overDT));
		await tick();
		expect(overDT.dropEffect).toBe('copy');

		const dropDT = readWriteDT();
		dropDT.setData(DRAG_MIME, ALPHA.npub);
		target.dispatchEvent(dragEvent('drop', dropDT));
		await tick();
		expect(await findByPlaceholderText('Name this group')).toBeTruthy();
	});
});
