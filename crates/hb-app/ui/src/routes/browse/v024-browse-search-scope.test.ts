// @vitest-environment jsdom
// v0.24.0 devtest item 7 — "searching in browse searches across all contacts instead of the
// selected contact. You should be able to search across grouped contacts as well." Scope ruling
// (owner, 2026-10-14): FOLLOW THE SELECTION — a selected contact scopes the search to them, a
// selected group (click its header) to its members, nothing selected = everyone; the strip says
// which, with a one-click way back to all.
//
// Mounted page with the REAL TitleSearch (not the q342 probe): only `$lib/api.js` is mocked, so
// the assertion is on the exact `searchTitles` call production makes after a real click.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { ContactSummary } from '$lib/types.js';
import { searchTitles, refreshContact, groupsGet } from '$lib/api.js';

vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn(),
	groupsCreateWithMembers: vi.fn(),
	groupsAssign: vi.fn(),
	groupsDelete: vi.fn(),
	groupsUnassign: vi.fn(),
	contactUpdateGroups: vi.fn(),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	applyKeyGrants: vi.fn().mockResolvedValue([]),
	searchTitles: vi.fn().mockResolvedValue({ hits: [], truncated: false }),
	follow: vi.fn(),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse') }) };
});
vi.mock('$app/stores', () => stubPage);

const searchMock = vi.mocked(searchTitles);
const A = 'npub1scopeaaaa' + 'q'.repeat(45);
const B = 'npub1scopebbbb' + 'q'.repeat(45);
const C = 'npub1scopecccc' + 'q'.repeat(45);

function peer(npub: string, name: string): ContactSummary {
	return {
		npub,
		has_browse_key: true,
		collections: [],
		online: false,
		last_fetched: '2026-08-01T00:00:00Z',
		local_tags: [],
		profile: { display_name: name, tags: [], languages: [], social_links: [], willing_to: [], updated: '2026-08-01T00:00:00Z' },
	} as unknown as ContactSummary;
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

async function setup() {
	vi.mocked(groupsGet).mockResolvedValue([{ name: 'Film club', pubkeys: [A, B] }]);
	vi.mocked(refreshContact).mockImplementation(async (npub: string) => [peer(A, 'Ana'), peer(B, 'Ben'), peer(C, 'Cy')].find((p) => p.npub === npub)!);
	contacts.set([peer(A, 'Ana'), peer(B, 'Ben'), peer(C, 'Cy')]);
	const r = render(BrowsePage);
	await waitFor(() => expect(r.getByRole('button', { name: 'Film club' })).toBeTruthy());
	return r;
}

async function searchFor(r: { getByPlaceholderText: (t: string) => HTMLElement }, q: string) {
	await fireEvent.input(r.getByPlaceholderText('Search titles…'), { target: { value: q } });
	await waitFor(() => expect(searchMock).toHaveBeenCalled());
}

describe('v0.24.0 item 7 — Browse title search follows the selection', () => {
	it('nothing selected searches every contact', async () => {
		const r = await setup();
		await searchFor(r, 'ghibli');
		expect(searchMock).toHaveBeenLastCalledWith('ghibli', 50);
		expect(r.queryByText(/Searching in/)).toBeNull();
	});

	it('a selected contact scopes the search to that contact', async () => {
		// P-10 MUTATION (orchestrator): in browse/+page.svelte searchScope, delete the
		// `if (selectedPeer) return …` line → REDS (the search goes to everyone).
		const r = await setup();
		await fireEvent.click(r.getByRole('button', { name: 'Cy' }));
		await searchFor(r, 'ghibli');
		expect(searchMock).toHaveBeenLastCalledWith('ghibli', 50, [C]);
		expect(r.getByText(/Searching in/).textContent).toContain('Cy');
	});

	it("clicking a group's header scopes the search to its members, and a second click releases it", async () => {
		// P-10 MUTATION (orchestrator): in browse/+page.svelte change the group header button's
		// `onclick={() => selectGroupScope(secGroup.name)}` to `onclick={() => {}}` → REDS.
		const r = await setup();
		await fireEvent.click(r.getByRole('button', { name: 'Cy' }));
		await fireEvent.click(r.getByRole('button', { name: 'Film club' }));
		await searchFor(r, 'ghibli');
		expect(searchMock).toHaveBeenLastCalledWith('ghibli', 50, [A, B]);
		expect(r.getByRole('button', { name: 'Film club' }).getAttribute('aria-pressed')).toBe('true');
		await fireEvent.click(r.getByRole('button', { name: 'Film club' }));
		// Released → back to the selected contact, re-run at once.
		await waitFor(() => expect(searchMock).toHaveBeenLastCalledWith('ghibli', 50, [C]));
	});

	it('"All contacts" widens back to everyone without leaving the contact', async () => {
		const r = await setup();
		await fireEvent.click(r.getByRole('button', { name: 'Cy' }));
		await searchFor(r, 'ghibli');
		await fireEvent.click(r.getByRole('button', { name: 'All contacts' }));
		await waitFor(() => expect(searchMock).toHaveBeenLastCalledWith('ghibli', 50));
		expect(r.queryByText(/Searching in/)).toBeNull();
		expect(r.getByRole('button', { name: 'Cy' }).classList.contains('contact-selected'), 'still browsing Cy').toBe(true);
	});
});
