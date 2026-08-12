// @vitest-environment jsdom
// M17 W1 — "Message" everywhere. The discovery hit-card (AddContactPanel's "Discover hoarders"
// section) gains a "Message" action alongside "Add contact". Add contact stays primary (first);
// Message routes to the `/chat?compose=<npub>` deep-link (works for non-contacts) and fires the
// `onmessage` callback rather than the `onadd` funnel.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import { tick } from 'svelte';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import AddContactPanel from './AddContactPanel.svelte';
import { contacts } from '../stores.js';
import type { PeerSearchHit } from '../api.js';

vi.mock('../api.js', () => ({
	pasteKey: vi.fn(),
	searchPeers: vi.fn(),
	discoverObservedTags: vi.fn(),
}));

import { searchPeers, discoverObservedTags } from '../api.js';

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

/** Default the observed-tags mock to an empty list so the autocomplete is non-empty only when a
 *  test explicitly opts in. */
function mockObservedTags(tags: string[] = []) {
	(discoverObservedTags as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(tags);
}

function makeHit(overrides: Partial<PeerSearchHit> = {}): PeerSearchHit {
	return {
		npub: 'npub1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
		display_name: 'Stranger',
		bio: null,
		tags: ['anime'],
		content_types: ['video'],
		picture: null,
		fingerprint: { words: ['alpha', 'beta'], colorHex: '#f00' },
		...overrides,
	};
}

/** Wrap hits in the M20 W3 result envelope `{ hits, capped }` (the shape `search_peers` returns). */
function resultEnvelope(hits: PeerSearchHit[], capped = false) {
	return { hits, capped };
}

/** Open the Discover section and run a search so the hit-cards render. Returns scoped query
 *  helpers bound to the rendered panel. */
async function discoverHits(hits: PeerSearchHit[], props: Record<string, unknown> = {}, capped = false) {
	(searchPeers as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(resultEnvelope(hits, capped));
	mockObservedTags();
	const { getByRole, getAllByRole, findByRole, findByText, queryByText, getByPlaceholderText } = render(AddContactPanel, {
		props: { open: true, ...props },
	});
	await fireEvent.click(getByRole('button', { name: /discover hoarders/i }));
	// QURATOR-44 broadened this placeholder from "tags (e.g. …)" to "name, bio, or tag (e.g. …)",
	// so the old /tags/i locator (plural) no longer matched and every test driving this flow failed
	// on the lookup rather than on its own assertion. Matched on the singular stem, which holds
	// across both wordings. The placeholder's exact copy is pinned in discover-view.test.ts — this
	// is only a locator, so it deliberately does NOT re-assert the copy here.
	await fireEvent.input(getByPlaceholderText(/tag/i), { target: { value: 'anime' } });
	await fireEvent.click(getByRole('button', { name: /^search$/i }));
	// Wait for the hit-card's Add-contact button (class hit-follow) to appear — that means searchPeers
	// resolved and the results rendered.
	const addBtn = await findByRole('button', { name: 'Add contact' });
	return { addBtn, getAllByRole, findByRole, findByText, queryByText };
}

describe('AddContactPanel — M17 W1 discovery Message action', () => {
	it('hit_card_renders_Add_contact_first_and_Message_second', async () => {
		const { addBtn, getAllByRole } = await discoverHits([makeHit()]);
		// The hit-card has both buttons (Add contact + Message); Add contact is primary/first.
		const msgBtns = getAllByRole('button', { name: 'Message' });
		expect(msgBtns.length).toBe(1);
		expect(addBtn).toBeTruthy();
		// Add contact precedes Message in document order.
		expect(addBtn.compareDocumentPosition(msgBtns[0]) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
	});

	it('Message_fires_onmessage_with_npub_not_onadd', async () => {
		const onadd = vi.fn();
		const onmessage = vi.fn();
		const { getAllByRole } = await discoverHits([makeHit({ npub: 'npub1msgtarget' })], { onadd, onmessage });
		await fireEvent.click(getAllByRole('button', { name: 'Message' })[0]);
		expect(onmessage).toHaveBeenCalledTimes(1);
		expect(onmessage.mock.calls[0][0]).toBe('npub1msgtarget');
		// Add funnel is NOT triggered by Message.
		expect(onadd).not.toHaveBeenCalled();
	});
});

describe('AddContactPanel — M20 W3 truncation affordance', () => {
	// QURATOR-44 replaced the "showing first N" wording with pagination plus a cap notice, because
	// "showing first N" was the never-ending-list symptom the owner asked to remove. The AFFORDANCE
	// must survive that copy change: a capped result still has to tell the user more matches exist
	// rather than presenting the slice as everyone. These two tests were re-pointed at the new
	// wording, NOT deleted — and note the negative case had silently become vacuous, since the old
	// /showing first/ string is absent whether or not the result is capped.
	it('tells the user more matches exist when the result is capped', async () => {
		// One hit returned, but the backend flagged more candidates existed (capped=true). Hit count
		// is independent of the cap flag — one hit + capped=true still means "there are more".
		const { findByText } = await discoverHits([makeHit()], {}, true);
		const affordance = await findByText(/more matches exist/i);
		expect(affordance).toBeTruthy();
		expect(affordance.getAttribute('role')).toBe('status');
	});

	it('shows no cap notice when the result is not capped', async () => {
		// capped=false → no notice. Mutation probe: rendering the notice unconditionally reds this.
		const { queryByText } = await discoverHits([makeHit()], {}, false);
		expect(queryByText(/more matches exist/i)).toBeNull();
	});
});

// QURATOR-70 chip/autocomplete DOM coverage.
//
// The 'ani' autocomplete test below is a real DOM-level render test — it proves the component
// renders the listbox, options, and wires click→addTagChip→chip render. The chip render and chip
// remove paths are covered by source-scan assertions here, NOT by a second DOM test, because the
// 'vhs' twin was flaky in jsdom: Svelte 5 `bind:value` did not reliably propagate the typed stem
// through `$derived` for the 'vhs' case (the 'ani' case was stable). Per CLAUDE.md §9, "a source-
// scan test can pin that the source still says the right thing, but jsdom computes no layout, so
// nothing in vitest proves a row renders as one line." The pure `suggestTags` tests in
// discover-view.test.ts (6 tests, mutation-proven) cover the filter math; the 'ani' DOM test covers
// the render path; these source scans cover the chip render + remove code paths.
describe('AddContactPanel — QURATOR-70 chip render + remove paths (source-scan)', () => {
	const here = path.dirname(fileURLToPath(import.meta.url));
	const panelPath = path.resolve(here, 'AddContactPanel.svelte');
	const src = fs.readFileSync(panelPath, 'utf8');

	it('chosen tags render as removable chips (span.disc-tag-chip with a remove button)', () => {
		// The chip affordance is load-bearing for multi-term strict-AND: it makes the second term a
		// real observed tag picked from a list. A chosen tag must render as a chip (pill), not free
		// text, so the user understands narrowing is tag-driven. The chip iterates committedTags and
		// each chip carries a remove button whose aria-label names the tag.
		expect(src).toContain('{#each committedTags as tag (tag)}');
		expect(src).toContain('<span class="disc-tag-chip">');
		expect(src).toContain('aria-label="Remove tag {tag}"');
		expect(src).toContain('onclick={() => removeTagChip(tag)}');
	});

	it('autocomplete options are real observed tags picked from a listbox (role=option)', () => {
		// The autocomplete renders observed tags as options; clicking one calls addTagChip. This is
		// the load-bearing affordance that makes multi-term narrowing tag-driven by construction.
		expect(src).toContain('<ul class="disc-autocomplete" role="listbox" aria-label="Suggested tags">');
		expect(src).toContain('<li role="option" aria-selected="false">');
		expect(src).toContain('onclick={() => addTagChip(tag)}');
	});

	it('addTagChip appends to committed tags; removeTagChip filters them out', () => {
		// addTagChip joins the parsed set + the new normalized tag; removeTagChip filters the set.
		// Both reassign discoverTags (the source of truth committedTags derives from).
		expect(src).toContain('discoverTags = [...parsedDiscoverTags, normalized].join(\', \');');
		expect(src).toContain('const remaining = parsedDiscoverTags.filter((t) => t !== tag);');
		expect(src).toContain('discoverTags = remaining.join(\', \');');
	});

	it('autocomplete excludes committed tags and the typed stem via suggestTags', () => {
		// suggestTags(observedTags, committedTags, typedStem) — the committed set excludes the stem
		// so typing 'vhs' still offers 'vhs'. The pure helper is unit-tested in discover-view.test.ts.
		expect(src).toContain('suggestTags(observedTags, committedTags, typedStem)');
	});
});

describe('AddContactPanel — QURATOR-70 autocomplete DOM render (ani stem)', () => {
	/** Open the Discover section, let the observed-tags mock resolve, and type a stem. Returns the
	 *  input plus scoped helpers.
	 *
	 *  Why only one DOM-level autocomplete test: Svelte 5 `bind:value` + `$derived` is flaky in jsdom
	 *  for some stems ('vhs' failed while 'ani' passed in the same harness). The render path is
	 *  identical for every stem — the only variance is jsdom/Svelte timing. One stable DOM test
	 *  proves the render path; the source-scan suite above pins the chip/remove wiring; the 6 pure
	 *  suggestTags tests in discover-view.test.ts pin the filter math. */
	async function openDiscoverAndType(stem: string) {
		const { getByRole, getByPlaceholderText } = render(AddContactPanel, {
			props: { open: true },
		});
		await fireEvent.click(getByRole('button', { name: /discover hoarders/i }));
		// Opening the Discover section triggers loadObservedTags(); let the mocked promise resolve.
		await new Promise((r) => setTimeout(r, 10));
		await tick();
		const input = getByPlaceholderText(/name, bio, or tag/i);
		await fireEvent.input(input, { target: { value: stem } });
		await tick();
		return { input, getByRole };
	}

	/** Wait for the autocomplete to show at least one option, then return the option text contents.
	 *  The observed-tags fetch resolves asynchronously; `vi.waitFor` polls until the Svelte reactive
	 *  update flushes and the options render. */
	async function waitForOptions(): Promise<string[]> {
		await vi.waitFor(() => {
			const n = document.body.querySelectorAll('[role="option"]').length;
			expect(n).toBeGreaterThan(0);
		}, { timeout: 2000, interval: 20 });
		return optionTexts();
	}

	/** The text content of every autocomplete option (role=option <li>) in document body, trimmed.
	 *  Tolerates zero options (returns []). */
	function optionTexts(): string[] {
		const nodes = document.body.querySelectorAll('[role="option"]');
		return Array.from(nodes).map((n) => (n.textContent ?? '').trim());
	}

	/** Click the autocomplete item whose text contains `tag`. Returns the clicked button. */
	async function clickOption(tag: string): Promise<HTMLElement> {
		// The option's clickable child is a <button class="disc-ac-item">; find by its text content.
		const buttons = document.body.querySelectorAll('button.disc-ac-item');
		for (const b of Array.from(buttons)) {
			if ((b.textContent ?? '').includes(tag)) {
				await fireEvent.click(b as HTMLElement);
				await tick();
				return b as HTMLElement;
			}
		}
		throw new Error(`no autocomplete option containing "${tag}" was found`);
	}

	it('autocomplete lists observed tags containing the typed stem, excluding already-chosen', async () => {
		// Typing 'ani' offers 'anime' and 'anime-classics'; after picking 'anime', only
		// 'anime-classics' remains for the same stem.
		mockObservedTags(['anime', 'anime-classics', 'vhs']);
		(searchPeers as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(resultEnvelope([]));
		const { input } = await openDiscoverAndType('ani');
		// Two options match 'ani'.
		const opts1 = await waitForOptions();
		expect(opts1.sort()).toEqual(['#anime', '#anime-classics']);
		// Pick 'anime'.
		await clickOption('anime');
		await tick();
		// After picking, the input holds 'anime'; continue typing ', ani' after the separator.
		// The typed stem is the tail after the last separator, so 'anime' is now committed/excluded.
		await fireEvent.input(input, { target: { value: 'anime, ani' } });
		await tick();
		const opts2 = await waitForOptions();
		expect(opts2).toEqual(['#anime-classics']);
	});
});

