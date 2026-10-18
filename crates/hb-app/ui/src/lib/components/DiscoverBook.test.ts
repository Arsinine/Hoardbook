// QURATOR-351 slice 5 — DiscoverBook MOUNT tests. $lib/api.js is mocked (the sibling Rust lane
// builds `discover_people`); everything else (Avatar, EmptyState, Modal, stores) is real.
// Every test names the exact production edit that must red it (P-10); each has been performed
// and the named test seen red.
// @vitest-environment jsdom
import { render, screen, fireEvent, waitFor, cleanup } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { discoverPeople, follow, goto } = vi.hoisted(() => ({
	discoverPeople: vi.fn(),
	follow: vi.fn(() => Promise.resolve()),
	goto: vi.fn(),
}));
vi.mock('$lib/api.js', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/api.js')>()),
	discoverPeople,
	follow,
}));
vi.mock('$app/navigation', () => ({ goto }));

import DiscoverBook from './DiscoverBook.svelte';
import type { PhonebookEntry } from '$lib/types.js';

function entry(over: Partial<PhonebookEntry>): PhonebookEntry {
	return {
		npub: 'npub' + Math.random().toString(36).slice(2, 10),
		display_name: 'Person',
		fingerprint: null,
		is_contact: false,
		total_bytes: 0,
		collections: [],
		tags: [],
		matched: [],
		...over,
	};
}

const book = [
	// Size order: Ben (2 GB) → Cy (1.5 GB) → Ana (3 MB). Ben carries NO likeness.
	entry({ npub: 'npubben', display_name: 'Ben Alvarez', total_bytes: 2e9 }),
	entry({
		npub: 'npubcy',
		display_name: 'Cy Young',
		total_bytes: 1.5e9,
		likeness: 40,
		is_contact: true,
		tags: ['film'],
		collections: [{ name: 'Kurosawa restorations', bytes: 5e8 }],
	}),
	entry({
		npub: 'npubana',
		display_name: 'Ana Sorel',
		total_bytes: 3e6,
		collections: [{ name: 'Field recordings', bytes: 3e6 }],
	}),
];

afterEach(cleanup);

beforeEach(() => {
	vi.clearAllMocks();
	discoverPeople.mockResolvedValue({ entries: book, capped: false, cold_start: false });
});

describe('DiscoverBook', () => {
	it('renders the book in size order and never shows % for a null likeness', async () => {
		// P-10 mutation (order): in lib/discover-book.ts flip the size comparator
		// `b.total_bytes - a.total_bytes || a.display_name...` → Ana renders before Ben → reds.
		// SEEN RED. (P-10 mutation (%): in DiscoverBook.svelte change the meta-line guard
		// `{#if e.likeness != null}` to `{#if true}` → Ben's card renders "null% like you" → reds.
		// SEEN RED.)
		const { container } = render(DiscoverBook);
		await waitFor(() => expect(screen.getByText('Ben Alvarez')).toBeTruthy());
		const order = [...container.querySelectorAll('article[data-npub]')].map((el) =>
			el.getAttribute('data-npub'),
		);
		expect(order).toEqual(['npubben', 'npubcy', 'npubana']);
		const ben = container.querySelector('article[data-npub="npubben"]');
		expect(ben?.textContent).not.toContain('%');
		const cy = container.querySelector('article[data-npub="npubcy"]');
		expect(cy?.textContent).toContain('40% like you');
	});

	it('drops the hoard line and swaps the Sort select for a two-button icon toggle', async () => {
		// MUTATION (P-10): change the likeness button's onclick to set 'size' → the press state
		// never flips and the ORDER never changes (Ben stays first) → the npubcy-first
		// assertion reds. Seen red (q384-mut.log, exit=1); restored → green.
		const { container } = render(DiscoverBook);
		await waitFor(() => expect(screen.getByText('Ben Alvarez')).toBeTruthy());
		// (a) the removed sentence is gone
		expect(container.textContent).not.toContain('biggest hoard first');
		// (b) no combobox named Sort; two pressed-state icon buttons instead
		expect(document.querySelectorAll('select[aria-label="Sort"]').length).toBe(0);
		const sizeBtn = screen.getByRole('button', { name: 'Sort by size' });
		const likeBtn = screen.getByRole('button', { name: 'Sort by likeness' });
		expect(sizeBtn.getAttribute('aria-pressed')).toBe('true');
		expect(likeBtn.getAttribute('aria-pressed')).toBe('false');
		expect(sizeBtn.querySelector('svg')).toBeTruthy();
		expect(likeBtn.querySelector('svg')).toBeTruthy();
		// (c) clicking likeness re-orders: size order Ben → Cy → Ana; likeness order puts
		// Cy (the only scored entry) first, null-likeness entries last.
		const cards = () =>
			[...container.querySelectorAll('article[data-npub]')].map((el) =>
				el.getAttribute('data-npub'),
			);
		expect(cards()[0]).toBe('npubben');
		fireEvent.click(likeBtn);
		expect(likeBtn.getAttribute('aria-pressed')).toBe('true');
		expect(sizeBtn.getAttribute('aria-pressed')).toBe('false');
		expect(cards()[0]).toBe('npubcy');
	});

	it('debounces the query, calls discoverPeople with it, and renders both groups', async () => {
		// P-10 mutation: in DiscoverBook.svelte change the fetch call
		// `discoverPeople(q ?? undefined)` to `discoverPeople()` → the query never reaches the
		// command → the toHaveBeenCalledWith('kurosawa') assertion reds. SEEN RED.
		discoverPeople.mockImplementation((q?: string) =>
			Promise.resolve(
				q
					? {
							entries: [
								entry({
									npub: 'nphana',
									display_name: 'Hana Petrov',
									total_bytes: 11e9,
									likeness: 84,
									matched: ['collection'],
									collections: [{ name: 'Kurosawa restorations', bytes: 4e9 }],
								}),
								entry({
									npub: 'npyusuf',
									display_name: 'Yusuf Demir',
									total_bytes: 14e9,
									likeness: 88,
									matched: ['looking_for'],
									looking_for: 'the Kurosawa Criterion box, region-free',
								}),
							],
							capped: false,
							cold_start: false,
						}
					: { entries: book, capped: false, cold_start: false },
			),
		);
		const { container } = render(DiscoverBook);
		await waitFor(() => expect(screen.getByText('Ben Alvarez')).toBeTruthy()); // book first
		fireEvent.input(screen.getByLabelText('Search the phonebook'), { target: { value: 'kurosawa' } });
		await waitFor(() => expect(discoverPeople).toHaveBeenCalledWith('kurosawa'), { timeout: 2000 });
		await waitFor(() => expect(screen.getByText('Has it')).toBeTruthy());
		expect(screen.getByText('Looking for it')).toBeTruthy();
		const groups = [...container.querySelectorAll('.res-title')].map((el) => el.textContent);
		expect(groups).toEqual(['Has it', 'Looking for it']); // Hana in Has-it; Yusuf in Looking-for-it
		expect(screen.getByText('Hana Petrov')).toBeTruthy();
		expect(screen.getByText('Yusuf Demir')).toBeTruthy();
	});

	it('flips a stranger to Contact the moment + is clicked (follow called)', async () => {
		// P-10 mutation: in DiscoverBook.svelte's addPerson comment out `addedLocally[npub] = true;`
		// → follow still fires but the chip never flips and the + button never leaves → the
		// toContain('Contact') assertion reds. SEEN RED.
		const { container } = render(DiscoverBook);
		await waitFor(() => expect(screen.getByText('Ana Sorel')).toBeTruthy());
		const anaCard = container.querySelector('article[data-npub="npubana"]') as HTMLElement;
		expect(anaCard.textContent).not.toContain('Contact');
		const addBtn = anaCard.querySelector('button[aria-label="Add to contacts"]');
		expect(addBtn).toBeTruthy();
		await fireEvent.click(addBtn as HTMLElement);
		await waitFor(() => expect(follow).toHaveBeenCalledWith('npubana'));
		await waitFor(() => expect(anaCard.textContent).toContain('Contact'));
		expect(anaCard.querySelector('button[aria-label="Add to contacts"]')).toBeNull(); // + gone
	});

	it('shows Message on contacts and strangers alike and composes to /chat', async () => {
		// P-10 mutation: in DiscoverBook.svelte's entryCard wrap the card's Message button in
		// `{#if !isContact(e)}…{/if}` → Cy (a contact) loses hers → the count-3 assertion reds.
		// SEEN RED.
		const { container } = render(DiscoverBook);
		await waitFor(() => expect(screen.getByText('Cy Young')).toBeTruthy());
		const msgs = [...container.querySelectorAll('article button')].filter(
			(b) => b.textContent === 'Message',
		);
		expect(msgs.length).toBe(3); // every entry, contact or not (owner ruling)
		const cyCard = container.querySelector('article[data-npub="npubcy"]') as HTMLElement;
		fireEvent.click(cyCard.querySelector('button.btn-ghost') as HTMLElement);
		await waitFor(() => expect(goto).toHaveBeenCalledWith('/chat?compose=npubcy'));
	});

	it('opens the person panel with Looking for as the FIRST section', async () => {
		// P-10 mutation: in DiscoverBook.svelte change the panel's `{#if panelEntry.looking_for}`
		// to `{#if false && panelEntry.looking_for}` → the Looking-for section vanishes → the
		// section-order assertions red. SEEN RED.
		discoverPeople.mockResolvedValue({
			entries: [
				entry({
					npub: 'npubelio',
					display_name: 'Elio Marchetti',
					total_bytes: 21e9,
					likeness: 91,
					looking_for: 'clean 35mm scans of Ran',
					tags: ['film restoration', 'world cinema'],
					collections: [
						{ name: 'Kurosawa restorations', bytes: 12.4e9 },
						{ name: 'Noir serials', bytes: 2.5e9 },
					],
				}),
			],
			capped: false,
			cold_start: false,
		});
		const { container } = render(DiscoverBook);
		await waitFor(() => expect(screen.getByText('Elio Marchetti')).toBeTruthy());
		fireEvent.click(container.querySelector('article[data-npub="npubelio"]') as HTMLElement);
		await waitFor(() => expect(container.querySelector('.modal-card')).toBeTruthy());
		const panel = container.querySelector('.modal-card') as HTMLElement;
		const text = panel.textContent ?? '';
		const at = (s: string) => text.indexOf(s);
		expect(at('Looking for')).toBeGreaterThanOrEqual(0);
		expect(at('Public collections')).toBeGreaterThan(at('Looking for'));
		expect(at('Interests')).toBeGreaterThan(at('Public collections'));
		expect(text).toContain('Readable once your hoard reaches 21.0 GB'); // v5 size rule line
		expect(text).toContain('clean 35mm scans of Ran');
	});

	it('shows the Contact section only when the entry carries a hint', async () => {
		// P-10 mutations, both SEEN RED (mutate by LINE NUMBER in DiscoverBook.svelte, never a
		// grep anchor):
		//  (a) delete the `{#if panelEntry.contact_hint}` line AND its matching `{/if}` line
		//      (the section then renders unconditionally) → the WITHOUT-hint assert reds.
		//  (b) delete the whole 4-line `{#if panelEntry.contact_hint} … {/if}` section
		//      (guard + heading + text + {/if}) → the WITH-hint asserts red.
		discoverPeople.mockResolvedValue({
			entries: [
				entry({
					npub: 'npubhint',
					display_name: 'Nia Okafor',
					total_bytes: 5e9,
					contact_hint: 'signal first, then email',
				}),
				entry({ npub: 'npubnohint', display_name: 'Rex Bello', total_bytes: 5e9 }),
			],
			capped: false,
			cold_start: false,
		});
		const { container } = render(DiscoverBook);
		await waitFor(() => expect(screen.getByText('Nia Okafor')).toBeTruthy());
		fireEvent.click(container.querySelector('article[data-npub="npubhint"]') as HTMLElement);
		await waitFor(() => expect(container.querySelector('.modal-card')).toBeTruthy());
		const panelText = () =>
			(container.querySelector('.modal-card') as HTMLElement).textContent ?? '';
		expect(panelText().indexOf('Contact')).toBeGreaterThanOrEqual(0);
		expect(panelText()).toContain('signal first, then email');
		fireEvent.click(container.querySelector('article[data-npub="npubnohint"]') as HTMLElement);
		await waitFor(() => expect(panelText()).toContain('Rex Bello'));
		expect(panelText()).not.toContain('Contact');
	});
});
