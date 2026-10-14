// @vitest-environment jsdom
// QURATOR-368 slice d — hover-to-expand column.
//
// Owner rulings (2026-10-14): hover/focus on a card animates the column to the FIXED expanded
// width (300 → 540, floor-capped); the right pane shrinks by the same amount and never grows
// past its rest width; right-pane content keeps its rest width and is clipped, NOT dimmed.
// Moving between cards keeps it open; leaving the column reverses after 160 ms. Short
// hover-intent delay (120 ms). prefers-reduced-motion → instant. The inline Browse · Message ·
// ⋯ actions render only after the widening SETTLES (flex-basis transitionend). Hover NEVER
// changes the right pane; only click does.
//
// P-13: jsdom computes no layout. The 260ms cubic-bezier easing, the clip-vs-reflow of the pane
// content and the two-line row shape are CSS claims NOT proven here — the Playwright +
// getBoundingClientRect/getComputedStyle recipe carries them.
//
// Mounted, not a source-scan (P-4): only `$lib/api.js` is mocked, the roster is seeded into the
// `contacts` store, assertions target the RENDERED DOM. Fake timers start only after the render
// wait (waitFor polls on real timers).
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ContactsPage from './+page.svelte';
import { contacts, contactsLoadError } from '$lib/stores.js';
import { expandedColumnWidth } from '$lib/contacts-expand.js';
import type { ContactSummary, Collection, Profile } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	follow: vi.fn().mockResolvedValue(undefined),
	refreshContact: vi.fn().mockResolvedValue(undefined),
	unfollowContact: vi.fn().mockResolvedValue(undefined),
	setContactTags: vi.fn().mockResolvedValue(undefined),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn().mockResolvedValue(undefined),
	groupsDelete: vi.fn().mockResolvedValue(undefined),
	groupsAssign: vi.fn().mockResolvedValue(undefined),
	groupsUnassign: vi.fn().mockResolvedValue(undefined),
	groupsCreateWithMembers: vi.fn().mockResolvedValue(undefined),
	contactUpdateGroups: vi.fn().mockResolvedValue(undefined),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
	onlineCount: vi.fn().mockResolvedValue({ online: 0, fetched_at: new Date().toISOString(), relay_set: [], fresh: [] }),
	relayStatus: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	privateAudienceList: vi.fn().mockResolvedValue([]),
	privateAudienceSet: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import { goto } from '$app/navigation';

const A_NPUB = 'npub1q368a' + 'a'.repeat(50);
const B_NPUB = 'npub1q368b' + 'b'.repeat(50);
const SEEN_3H_AGO = new Date(Date.now() - 3 * 3_600_000).toISOString();

const PROF: Profile = {
	display_name: 'Dana Expandable',
	bio: 'Film archivist.',
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	total_bytes: 5_000_000_000,
	updated: '2026-08-01T00:00:00Z',
} as unknown as Profile;

function peer(npub: string, hasCols: boolean): ContactSummary {
	return {
		npub,
		has_browse_key: false,
		collections: hasCols
			? ([{ slug: 'films', path_alias: 'films', visibility: 'Public', est_size: '1.2 GB', description: 'Movies' }] as unknown as Collection[])
			: ([] as unknown as Collection[]),
		online: false,
		last_fetched: new Date().toISOString(),
		last_presence: SEEN_3H_AGO,
		local_tags: [],
		profile: PROF,
		fingerprint: { words: ['amber', 'otter', 'quill'], colorHex: '#8fa2ff' },
	} as unknown as ContactSummary;
}

/** jsdom computes no layout — hand the production measurement code its inputs directly. */
function stubWidth(el: Element, px: number): void {
	Object.defineProperty(el, 'clientWidth', { configurable: true, value: px });
}

function colOf(container: HTMLElement): HTMLElement {
	return container.querySelector('.contacts-col') as HTMLElement;
}
function cardOf(container: HTMLElement, npub: string): HTMLElement {
	return container.querySelector(`[data-npub="${npub}"]`) as HTMLElement;
}

interface Rig {
	container: HTMLElement;
	col: HTMLElement;
	body: HTMLElement;
}

/**
 * Seeds two contacts (A has a collection, B has none), renders the page into RTL's own
 * container, waits for the roster, and stubs the layout the production code measures.
 * `restPane` is the right pane's rest width implied by the stubbed body width (gap is 0 in
 * jsdom): restPane = bodyWidth − 300.
 */
async function setup(bodyWidth: number): Promise<Rig> {
	contacts.set([peer(A_NPUB, true), peer(B_NPUB, false)]);
	const { container } = render(ContactsPage);
	await waitFor(() => {
		expect(container.querySelector('.contact-card')).not.toBeNull();
	});
	const col = colOf(container);
	const body = col.parentElement as HTMLElement;
	stubWidth(body, bodyWidth);
	stubWidth(col, 300);
	return { container, col, body };
}

/** pointerover the named card, wait out the 120ms intent. Fake timers must be active. */
async function hoverA(container: HTMLElement): Promise<void> {
	fireEvent.pointerOver(cardOf(container, A_NPUB));
	await vi.advanceTimersByTimeAsync(120);
	await tick();
}

afterEach(() => {
	cleanup();
	vi.useRealTimers();
	vi.unstubAllGlobals();
	vi.clearAllMocks();
	contacts.set([]);
	contactsLoadError.set(false);
});

function fireFlexBasisEnd(col: HTMLElement): void {
	const te = new Event('transitionend', { bubbles: true });
	Object.defineProperty(te, 'propertyName', { value: 'flex-basis' });
	fireEvent(col, te);
}

describe('QURATOR-368d — the width cap (pure helper)', () => {
	it('full width when the floor is respected', () => {
		expect(expandedColumnWidth(810)).toBe(540);
		expect(expandedColumnWidth(560)).toBe(540); // exactly 320 left for the pane
	});
	it('capped when the floor would be violated', () => {
		expect(expandedColumnWidth(460)).toBe(440); // pane keeps exactly 320
	});
	it('null when no expansion fits', () => {
		expect(expandedColumnWidth(300)).toBeNull(); // no room to grow at all
		expect(expandedColumnWidth(310)).toBeNull();
		expect(expandedColumnWidth(0)).toBeNull();
	});
});

describe('QURATOR-368d — hover-to-expand column', () => {
	it('pointerover a card expands the column after the 120ms intent, at the fixed expanded width', async () => {
		const { container, col } = await setup(1110); // rest pane 810 → full 540
		vi.useFakeTimers();
		fireEvent.pointerOver(cardOf(container, A_NPUB));
		await vi.advanceTimersByTimeAsync(119);
		await tick();
		// P-10 MUTATION (orchestrator): in +page.svelte change `HOVER_INTENT_MS = 120` to
		// `HOVER_INTENT_MS = 200` → REDS (column NOT expanded after 120 ms of hover).
		expect(col.classList.contains('col-expanded'), 'not expanded before the intent elapses').toBe(false);
		expect(col.style.flexBasis, 'no inline basis before the intent elapses').toBe('');

		await vi.advanceTimersByTimeAsync(1);
		await tick();
		expect(col.classList.contains('col-expanded'), 'expanded after 120ms of hover').toBe(true);
		expect(col.style.flexBasis, 'expanded basis is the fixed 540px').toBe('540px');
	});

	it('moving to a second card keeps the column open; hover never changes the pane (only click does)', async () => {
		const { container, col } = await setup(1110);
		vi.useFakeTimers();

		// Select the FIRST contact by CLICK — the owner gate: only click changes the pane.
		fireEvent.click(cardOf(container, A_NPUB));
		await vi.advanceTimersByTimeAsync(250); // the held row-select timer
		await tick();
		expect(container.querySelector('.pane-name')?.textContent).toBe('Dana Expandable');

		fireEvent.pointerOver(cardOf(container, B_NPUB));
		await vi.advanceTimersByTimeAsync(120);
		await tick();
		expect(col.classList.contains('col-expanded'), 'still expanded').toBe(true);
		expect(container.querySelector('.pane-name')?.textContent, 'pane still shows the clicked contact').toBe('Dana Expandable');
		expect(vi.mocked(goto)).not.toHaveBeenCalled();
	});

	it('leaving the column collapses it after the 160ms close delay', async () => {
		const { container, col } = await setup(1110);
		vi.useFakeTimers();
		await hoverA(container);
		expect(col.classList.contains('col-expanded')).toBe(true);

		fireEvent.pointerLeave(col);
		await vi.advanceTimersByTimeAsync(159);
		await tick();
		// P-10 MUTATION (orchestrator): in +page.svelte change `HOVER_CLOSE_MS = 160` to
		// `HOVER_CLOSE_MS = 1600` → REDS (already collapsed at 159–160 ms of leave).
		expect(col.classList.contains('col-expanded'), 'still expanded at 159ms of leave').toBe(true);

		await vi.advanceTimersByTimeAsync(1);
		await tick();
		expect(col.classList.contains('col-expanded'), 'collapsed after 160ms').toBe(false);
		expect(col.classList.contains('col-settled')).toBe(false);
		expect(col.style.flexBasis, 'inline basis removed on collapse').toBe('');
	});

	it('keyboard focus (focusin) on a card expands too', async () => {
		const { container } = await setup(1110);
		vi.useFakeTimers();
		fireEvent.focusIn(cardOf(container, A_NPUB));
		await vi.advanceTimersByTimeAsync(120);
		await tick();
		expect(colOf(container).classList.contains('col-expanded'), 'focus expands the column').toBe(true);
	});

	it('inline Browse · Message · ⋯ render only in the SETTLED state (after the flex-basis transitionend)', async () => {
		const { container, col } = await setup(1110);
		vi.useFakeTimers();
		await hoverA(container);
		expect(col.classList.contains('col-expanded')).toBe(true);
		expect(col.querySelector('.row-acts'), 'no actions before the widening settles').toBeNull();

		fireFlexBasisEnd(col);
		await tick();
		// P-10 MUTATION (orchestrator): in +page.svelte onColExpandTransitionEnd remove the
		// `e.propertyName !== 'flex-basis'` guard (or render .row-acts off hotNpub alone) →
		// REDS (actions visible before any transitionend).
		const acts = col.querySelector('.row-acts');
		expect(acts, 'actions render once settled').not.toBeNull();
		expect(
			[...(acts as HTMLElement).querySelectorAll('button')].map((b) => b.textContent?.trim()),
		).toEqual(['Browse', 'Message', '⋯']);
		// Owner: "the row no longer grows on hover" — the actions sit at the END OF LINE 1 (draft
		// `.l1 … .acts-in`), never as a third line. P-10 MUTATION (orchestrator): move the
		// `{#if colSettled && hotNpub === peer.npub}` block out of `.name-row` to after it → REDS.
		const card = (acts as HTMLElement).closest('.contact-card') as HTMLElement;
		expect((acts as HTMLElement).parentElement?.classList.contains('name-row'), 'actions live on line 1').toBe(true);
		const lines = [...card.querySelector('.contact-info')!.children].filter((el) => el.tagName === 'DIV');
		expect(lines.length, 'the expanded row is still exactly two lines').toBe(2);
	});

	it('Browse is absent for a no-collection contact; Message and ⋯ remain', async () => {
		const { container, col } = await setup(1110);
		vi.useFakeTimers();
		await hoverA(container);
		fireFlexBasisEnd(col);
		await tick();

		// Move the hover to the no-collection contact — the column STAYS open and settled.
		fireEvent.pointerOver(cardOf(container, B_NPUB));
		await tick();
		const rowBacts = cardOf(container, B_NPUB).querySelector('.row-acts');
		expect(rowBacts, 'B shows its actions while settled').not.toBeNull();
		expect(rowBacts!.querySelector('button')?.textContent, 'B first act is Message, NOT Browse').toContain('Message');
		expect(rowBacts!.textContent, 'no Browse without a collection').not.toContain('Browse');
		expect(cardOf(container, A_NPUB).querySelector('.row-acts'), 'the hot card moved').toBeNull();
	});

	it('prefers-reduced-motion settles immediately on expand', async () => {
		vi.stubGlobal('matchMedia', vi.fn(() => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
		const { container, col } = await setup(1110);
		vi.useFakeTimers();
		await hoverA(container);
		// P-10 MUTATION (orchestrator): in openHoverNow delete the `if (expandMq?.matches)
		// colSettled = true;` line → REDS (col-settled absent right after the intent elapses).
		expect(col.classList.contains('col-settled'), 'settled without any transitionend').toBe(true);
		expect(col.querySelector('.row-acts'), 'actions render immediately').not.toBeNull();
	});

	it('floor: the expansion is capped so the pane keeps its 320px floor', async () => {
		const { container, col } = await setup(760); // rest pane 460 → capped 440
		vi.useFakeTimers();
		await hoverA(container);
		// P-10 MUTATION (orchestrator): in +page.svelte openHoverNow, expand unconditionally to
		// 540 (drop the `w === null` floor logic AND the cap) → REDS (basis would be 540, not 440).
		expect(col.style.flexBasis, 'capped to 440 so the pane keeps 320').toBe('440px');
	});

	it('when no expansion fits at all, there is no expanded class', async () => {
		const { container, col } = await setup(600); // rest pane 300 → nothing fits → no expansion
		vi.useFakeTimers();
		await hoverA(container);
		// P-10 MUTATION (orchestrator): in +page.svelte openHoverNow delete the
		// `if (w === null) return;` line → REDS (col-expanded present despite no fit).
		expect(col.classList.contains('col-expanded'), 'no expansion when none fits').toBe(false);
		expect(col.style.flexBasis).toBe('');
	});

	it('the column cannot be stretched by its content (min-width: 0 on the column and the phonebook)', () => {
		// Orchestrator review, 2026-10-14 — measured in Chromium: without min-width: 0, a flex item's
		// min-width is `auto` (its min-content), so ONE long contact name widened the column to 486px
		// at rest and 664px on hover (past the 540 cap) instead of ellipsising. The widths were proven
		// with the vite + Playwright recipe (300 rest / 540 hover with a 52-char name, row stays two
		// lines). WHY A SOURCE SCAN (P-4 note): mounting was tried first — in this vitest setup the
		// component's <style> is not injected into jsdom, so getComputedStyle(col).minWidth reads ''
		// for EVERY rule, fixed or not. The scan slices out the exact rule bodies and strips comments
		// (P-12), so the comment that explains the fix cannot satisfy it.
		// P-10 MUTATION (orchestrator): delete `min-width: 0;` from `.contacts-col` → REDS.
		// Resolved from cwd (vitest root = crates/hb-app/ui): under jsdom import.meta.url is http://.
		const src = readFileSync(resolve(process.cwd(), 'src/routes/contacts/+page.svelte'), 'utf8');
		const css = src.slice(src.indexOf('<style>')).replace(/\/\*[\s\S]*?\*\//g, '');
		const rule = (sel: string) => {
			const m = css.match(new RegExp('\\n\\s*' + sel.replace('.', '\\.') + '\\s*\\{([^}]*)\\}'));
			expect(m, `${sel} rule exists`).not.toBeNull();
			return (m as RegExpMatchArray)[1];
		};
		expect(rule('.contacts-col'), '.contacts-col').toMatch(/(^|;|\s)min-width:\s*0(px)?\s*;/);
		expect(rule('.phonebook'), '.phonebook').toMatch(/(^|;|\s)min-width:\s*0(px)?\s*;/);
	});
});
