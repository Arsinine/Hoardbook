// @vitest-environment jsdom
// QURATOR-342 D1 — TitleSearch strip tests. Mounts the real component, mocks ONLY `../api.js` —
// the `contacts` store is the REAL store, seeded per test with `contacts.set()`.
//
// Per CLAUDE.md §9 every test here was SEEN RED: the exact production edit that reds each is in
// the comment beside the test (and in the lane report). Mutation runs: the driver at
// scratchpad/d1_mutation_driver.py, per-mutation logs d1-m{1..10}.log (order dir).
//
// jsdom computes no layout — nothing here proves a row renders on one line; only that the copy,
// the conditions and the call sequence are right.
import { describe, it, expect, vi, afterEach } from 'vitest';
import type { ContactSummary } from '../types.js';
import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';

vi.mock('../api.js', () => ({
	searchTitles: vi.fn(),
	follow: vi.fn().mockResolvedValue(undefined),
	getContacts: vi.fn().mockResolvedValue([]),
}));

import { searchTitles, follow, getContacts } from '../api.js';
import { contacts } from '../stores.js';
import TitleSearch from './TitleSearch.svelte';

const searchMock = searchTitles as unknown as ReturnType<typeof vi.fn>;
const followMock = follow as unknown as ReturnType<typeof vi.fn>;
const getContactsMock = getContacts as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	vi.useRealTimers();
	contacts.set([]);
});

const MARA = { npub: 'npub1maraqqqq', display_name: 'Mara Vela', slug: 'films', path: 'ghibli' };
const JONAS = { npub: 'npub1jonasqqq', display_name: 'Jonas Ketter', slug: 'films', path: 'ghibli' };
const CONTACT = { npub: 'npub1contactq', display_name: 'Already Contact', slug: 'films', path: 'ghibli' };

const GHIBLI = {
	title: 'Studio Ghibli — Complete Films',
	name_norm: 'studio ghibli complete films',
	holder_count: 3,
	holders: [MARA, JONAS, CONTACT],
};

function hitsResult(hits: unknown[], truncated = false) {
	return { hits, truncated };
}

function type(text: string) {
	const input = screen.getByPlaceholderText('Search titles…') as HTMLInputElement;
	return fireEvent.input(input, { target: { value: text } });
}

/** Render once, type once, wait for the debounced answer. The ONLY place a mount happens. */
async function mount(hits: unknown[], truncated = false, onbrowse: (npub: string) => void = vi.fn()) {
	render(TitleSearch, { props: { onbrowse } });
	searchMock.mockResolvedValue(hitsResult(hits, truncated));
	await type('ghibli');
	await screen.findAllByText(GHIBLI.title);
}

function rowFor(name: string) {
	return screen.getByText(name).closest('.ts-holder') as HTMLElement;
}

function btnNames(el: HTMLElement) {
	return Array.from(el.querySelectorAll('button')).map((b) => b.textContent?.trim());
}

function summary(npub: string, displayName?: string): ContactSummary {
	return {
		npub,
		has_browse_key: true,
		collections: [],
		online: false,
		last_fetched: '',
		local_tags: [] as string[],
		profile: displayName
			? ({ display_name: displayName } as unknown as ContactSummary['profile'])
			: undefined,
	};
}

/** Open the first result's <details> and return the details element. */
async function expandFirst() {
	const summaryEl = screen.getAllByText(GHIBLI.title)[0].closest('summary') as HTMLElement;
	const details = summaryEl.parentElement as HTMLDetailsElement;
	if (!details.open) await fireEvent.click(summaryEl);
	return details;
}

describe('TitleSearch', () => {
	it('debounced_search_runs_once_for_fast_typing', async () => {
		// RED by: onInput() calling runSearch(q) immediately instead of scheduling the 250 ms debounce.
		searchMock.mockResolvedValue(hitsResult([]));
		vi.useFakeTimers();
		render(TitleSearch, { props: { onbrowse: vi.fn() } });
		await type('g');
		await type('gh');
		await type('ghi');
		await type('ghib');
		await type('ghibli');
		expect(searchMock).not.toHaveBeenCalled(); // nothing fired mid-burst
		await vi.advanceTimersByTimeAsync(300);
		expect(searchMock).toHaveBeenCalledTimes(1); // exactly one ask for the whole burst
		expect(searchMock).toHaveBeenCalledWith('ghibli', 50);
	});

	it('result_row_shows_title_and_holder_count', async () => {
		// RED by: dropping {hit.holder_count} from the .ts-count span (title alone stays green).
		await mount([GHIBLI]);
		expect(screen.getByText(GHIBLI.title)).toBeTruthy();
		expect(screen.getByText('3 people have this')).toBeTruthy();
	});

	it('expanding_row_shows_bare_holders', async () => {
		// RED by: emptying the holders {#each hit.holders …} array (rows never render).
		await mount([GHIBLI]);
		const details = await expandFirst();
		expect(details.open).toBe(true); // collapsed until the summary is clicked
		// Holder rows stay BARE (owner ruling): name + slug/path only — no overlap/similarity text.
		expect(rowFor('Mara Vela')).toBeTruthy();
		expect(rowFor('Mara Vela').textContent).toContain('films/ghibli');
		expect(screen.queryByText(/overlap/i)).toBeNull();
	});

	it('browse_button_calls_onbrowse_with_holder_npub', async () => {
		// RED by: the Browse button calling onbrowse() with no argument.
		const onbrowse = vi.fn();
		await mount([GHIBLI], false, onbrowse);
		await expandFirst();
		// Mara's row's Browse (every holder row has one).
		const browseMara = Array.from(rowFor('Mara Vela').querySelectorAll('button')).find(
			(b) => b.textContent?.trim() === 'Browse',
		) as HTMLButtonElement;
		await fireEvent.click(browseMara);
		expect(onbrowse).toHaveBeenCalledWith('npub1maraqqqq');
	});

	it('add_shown_only_for_non_contacts_and_adds', async () => {
		// RED by: inverting the contact check ({#if $contacts.some(…)}) so "+ Add" renders for
		// existing contacts too (and "Added ✓" never does).
		contacts.set([summary(CONTACT.npub, 'Already Contact')]);
		// The post-add refresh: the roster now also holds Mara, so her row flips to "Added ✓".
		getContactsMock.mockResolvedValue([summary(CONTACT.npub, 'Already Contact'), summary(MARA.npub)]);
		await mount([GHIBLI]);
		await expandFirst();
		// Per-ROW check: a contact row carries only Browse (+ "Added ✓"); a stranger row carries
		// Browse + "+ Add" (owner ruling: adding is not reading).
		expect(btnNames(rowFor('Already Contact'))).toEqual(['Browse']);
		expect(rowFor('Already Contact').textContent).toContain('Added ✓');
		expect(btnNames(rowFor('Mara Vela'))).toEqual(['Browse', '+ Add']);
		const addMara = Array.from(rowFor('Mara Vela').querySelectorAll('button')).find(
			(b) => b.textContent?.trim() === '+ Add',
		) as HTMLButtonElement;
		await fireEvent.click(addMara);
		await waitFor(() => expect(followMock).toHaveBeenCalledWith('npub1maraqqqq'));
		// After the add, the store refresh flips the row to "Added ✓" — no per-row local state.
		await waitFor(() => expect(screen.getAllByText('Added ✓').length).toBe(2));
		// Jonas (still a stranger) keeps his "+ Add".
		expect(screen.getAllByRole('button', { name: '+ Add' }).length).toBe(1);
	});

	it('holder_cap_line_when_more_holders_than_listed', async () => {
		// RED by: deleting the holder-cap {#if hit.holder_count > hit.holders.length} block.
		await mount([{ ...GHIBLI, holder_count: 34, holders: [MARA, JONAS] }]);
		await expandFirst();
		expect(screen.getByText('Showing first 2 of 34 holders')).toBeTruthy();
	});

	it('truncated_line_when_more_titles_than_shown', async () => {
		// RED by: deleting the {#if truncated} foot block.
		await mount([GHIBLI, GHIBLI, GHIBLI], true);
		expect(screen.getByText('3 titles shown — refine the search to see more')).toBeTruthy();
	});

	it('error_shows_retry_never_no_results', async () => {
		// RED by: the catch branch setting status = 'done' (a failed search rendering as "no results").
		searchMock.mockRejectedValue(new Error('relay down'));
		render(TitleSearch, { props: { onbrowse: vi.fn() } });
		await type('ghibli');
		await waitFor(() => expect(screen.getByText(/Search failed/)).toBeTruthy());
		expect(screen.getByRole('button', { name: 'Retry' })).toBeTruthy();
		expect(screen.queryByText('No titles match your search.')).toBeNull();
		// Retry works: the next answer lands as results, not another error.
		searchMock.mockResolvedValue(hitsResult([GHIBLI]));
		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		await waitFor(() => expect(screen.getByText(GHIBLI.title)).toBeTruthy());
	});

	it('empty_query_renders_nothing_but_the_hint', async () => {
		// RED by: deleting the !q reset branch in onInput (stale results surviving a cleared input).
		await mount([GHIBLI]);
		await type('');
		await tick();
		expect(screen.queryByText(GHIBLI.title)).toBeNull();
		expect(screen.queryByText('No titles match your search.')).toBeNull();
		expect(screen.queryByText('Searching…')).toBeNull();
		expect(screen.getByText('Counts cover only people you can read.')).toBeTruthy();
	});

	it('copy_never_says_download', async () => {
		// RED by: renaming any rendered button/copy to "Download" (e.g. the Browse button label).
		await mount([{ ...GHIBLI, holder_count: 34, holders: [MARA] }], true);
		await expandFirst();
		const text = document.body.textContent ?? '';
		expect(text).toMatch(/Counts cover only people you can read\./);
		expect(text).not.toMatch(/download/i);
	});
});
