// @vitest-environment jsdom
// Owner feedback #6 — Browse carries a Chat-style Refresh button in its topbar. Behavioural mount
// test: the button must exist with an accessible name, and clicking it must re-run the page's
// EXISTING loaders (loadContactsInto/getContacts, loadGroupsInto, loadPrivateInto) — asserted on
// the mocked api call counts, never on internals.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The mutation
// probe (point the button's onclick at a no-op, re-run this file) MUST fail both tests here.
//
// jsdom computes no layout — nothing here proves the button RENDERS in the topbar, only that it
// exists, is named, and is wired to the loaders.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';

// Same stub set as q92-private-collections.test.ts, plus getContacts/applyKeyGrants (the refresh
// path re-pulls contacts through loadContactsInto, and mount's loadKeyGrants reads applyKeyGrants).
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
	applyKeyGrants: vi.fn().mockResolvedValue([]),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

// $page stub: plain /browse, no ?peer deep-link (q92's shape, minus the query param).
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse') }) };
});
vi.mock('$app/stores', () => stubPage);

import { browsePrivateCollections, groupsGet, getContacts } from '$lib/api.js';
const privateMock = browsePrivateCollections as unknown as ReturnType<typeof vi.fn>;
const groupsMock = groupsGet as unknown as ReturnType<typeof vi.fn>;
const contactsMock = getContacts as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('Owner feedback #6 — Browse topbar Refresh button', () => {
	it('renders with an accessible name and re-runs the page loaders on click', async () => {
		const { getByRole } = render(BrowsePage);
		await tick();
		// Mount effects: private collections + groups each load once. getContacts is NOT called on
		// mount (no grants applied, no retry), so its baseline is zero.
		await waitFor(() => expect(privateMock).toHaveBeenCalledTimes(1));
		await waitFor(() => expect(groupsMock).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 50)); // let mount settle

		const btn = getByRole('button', { name: 'Refresh collections' }) as HTMLButtonElement;
		expect(btn.getAttribute('title')).toBe('Refresh collections');
		await fireEvent.click(btn);
		await tick();

		// THE assertions that matter: one click re-pulled every store the page paints from.
		await waitFor(() => expect(privateMock).toHaveBeenCalledTimes(2));
		await waitFor(() => expect(groupsMock).toHaveBeenCalledTimes(2));
		await waitFor(() => expect(contactsMock).toHaveBeenCalledTimes(1));
	});

	it('the button is disabled while a refresh is in flight (cannot double-fire)', async () => {
		let release: (() => void) | undefined;
		privateMock.mockImplementation(() => new Promise((r) => { release = () => r([]); }));
		const { getByRole } = render(BrowsePage);
		await tick();
		await waitFor(() => expect(privateMock).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 50));

		const btn = getByRole('button', { name: 'Refresh collections' }) as HTMLButtonElement;
		expect(btn.disabled).toBe(false);
		await fireEvent.click(btn);
		await tick();
		// In flight: disabled until the loaders settle.
		await waitFor(() => expect(btn.disabled).toBe(true));
		release!();
		await waitFor(() => expect(btn.disabled).toBe(false));
	});
});
