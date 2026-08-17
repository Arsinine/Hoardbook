// @vitest-environment jsdom
// QURATOR-93 (minor-6) — the drag-group namer's migration to the shared `Modal` shell
// (q98-drag-namer-modal.test.ts, source-scan) dropped its accessible name: the old hand-rolled
// `.dg-panel` carried `aria-label="Name this group"`, but `<Modal>` only wires `aria-labelledby`
// when a `title` is passed, and the namer passes none (a visible `<h2>` would collide with its own
// compact `padding="0"` dg-header — see the `ariaLabel` prop added to Modal.svelte). This is a
// BEHAVIOURAL mount test: the only way to see the dialog's COMPUTED accessible name is to actually
// open it, which the source-scan file cannot do.
//
// The namer opens via native HTML5 drag-and-drop OR the `g`/`G` keyboard shortcut once 2+ contacts
// are selected (M22 W7) — the keyboard path is used here since it needs no DataTransfer/drag-event
// simulation (there is no prior art for that in this codebase; jsdom does not implement DnD).
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code; the mutation
// probe (drop `ariaLabel` from the Modal call, re-run this file) is documented in the task report.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { CachedPeer } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	groupsGet: vi.fn().mockResolvedValue([]),
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

// No `?peer=` — this test never selects a peer into the right-hand detail panel, only into the
// People rail's multi-select (a different piece of state, `selectedNpubs`).
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse') }) };
});
vi.mock('$app/stores', () => stubPage);

const PROF = (name: string) => ({ display_name: name, tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' });

const PEER_A: CachedPeer = { npub: 'npub1dgnamera' + 'a'.repeat(50), collections: [], online: false, last_fetched: '2026-08-01T00:00:00Z', local_tags: [], profile: PROF('Alpha Peer') };
const PEER_B: CachedPeer = { npub: 'npub1dgnamerb' + 'b'.repeat(50), collections: [], online: false, last_fetched: '2026-08-01T00:00:00Z', local_tags: [], profile: PROF('Beta Peer') };

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

describe('QURATOR-93 (minor-6) — the drag-group namer dialog has an accessible name', () => {
	it('opening the namer via the g-key multi-select shortcut exposes a named dialog', async () => {
		contacts.set([PEER_A, PEER_B]);
		const { getByRole } = render(BrowsePage);
		await tick();

		// Select both rows: plain mousedown on the first (selection = [A]), ctrl-mousedown on the
		// second (meta-toggle ADDS to the selection — order-independent, unlike Shift-range).
		const rowA = getByRole('button', { name: /Alpha Peer/ });
		const rowB = getByRole('button', { name: /Beta Peer/ });
		await fireEvent.mouseDown(rowA);
		await fireEvent.mouseDown(rowB, { ctrlKey: true });
		await tick();

		// The g-key shortcut opens the SAME multi-select namer the drag gesture does (M22 W7).
		await fireEvent.keyDown(window, { key: 'g' });
		await tick();

		await waitFor(() => {
			expect(getByRole('dialog', { name: /name this group/i })).toBeTruthy();
		});
	});
});
