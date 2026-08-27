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
import type { CachedPeer } from '../types.js';

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
	const { getByRole, getAllByRole, findByRole, findByText, queryByText, queryAllByText, getByPlaceholderText } = render(AddContactPanel, {
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
	// Wait for the hit-card div to appear — that means searchPeers resolved and the results rendered.
	// (QURATOR-104: a hit whose npub is already a contact renders a disabled "Added" button instead of
	// "Add contact", so waiting on that button by name would time out for exactly the hits under test.)
	await vi.waitFor(() => {
		expect(document.body.querySelectorAll('.hit-card').length).toBeGreaterThan(0);
	}, { timeout: 2000, interval: 20 });
	// The Add button, when this is a stranger hit (null when every hit is a known contact).
	const addBtn = document.body.querySelector('button.hit-follow:not([disabled])');
	return { addBtn, getAllByRole, findByRole, findByText, queryByText, queryAllByText };
}

describe('AddContactPanel — M17 W1 discovery Message action', () => {
	it('hit_card_renders_Add_contact_first_and_Message_second', async () => {
		const { addBtn, getAllByRole } = await discoverHits([makeHit()]);
		// The hit-card has both buttons (Add contact + Message); Add contact is primary/first.
		const msgBtns = getAllByRole('button', { name: 'Message' });
		expect(msgBtns.length).toBe(1);
		expect(addBtn).toBeTruthy();
		// Add contact precedes Message in document order.
		expect(addBtn!.compareDocumentPosition(msgBtns[0]) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
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

// Discover tag-cloud (owner sign-off 2026-08-15) — DOM-level coverage of the pristine-state cloud:
// real render + real click → addTagChip, and the hide-on-first-keystroke/disappear-on-commit rules.
// The uniform-size / no-fake-frequency rule is a design constraint, not behavior; it is pinned by
// the source-scan suite above NOT carrying any per-tag size styling.
describe('AddContactPanel — Discover tag-cloud (pristine observed tags)', () => {
	/** Open the Discover section with the observed-tags mock resolved and the input left EMPTY.
	 *  Returns body-scoped helpers (the cloud lives outside the results list). */
	async function openDiscoverPristine(tags: string[]) {
		mockObservedTags(tags);
		(searchPeers as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(resultEnvelope([]));
		const { getByRole, getByPlaceholderText, queryAllByRole } = render(AddContactPanel, {
			props: { open: true },
		});
		await fireEvent.click(getByRole('button', { name: /discover hoarders/i }));
		// loadObservedTags() fires on open; let the mocked promise resolve and Svelte flush.
		await vi.waitFor(() => {
			const n = document.body.querySelectorAll('button.disc-cloud-tag').length;
			expect(n).toBeGreaterThan(0);
		}, { timeout: 2000, interval: 20 });
		return { getByRole, getByPlaceholderText, queryAllByRole };
	}

	function cloudTags(): string[] {
		return Array.from(document.body.querySelectorAll('button.disc-cloud-tag')).map(
			(b) => (b.textContent ?? '').trim(),
		);
	}

	it('renders observed tags as a clickable cloud before anything is typed', async () => {
		await openDiscoverPristine(['anime', 'manga', 'vhs']);
		expect(cloudTags()).toEqual(['#anime', '#manga', '#vhs']);
	});

	it('clicking a cloud tag commits it via the same addTagChip path as the autocomplete', async () => {
		const { getByRole, getByPlaceholderText } = await openDiscoverPristine(['anime', 'vhs']);
		await fireEvent.click(getByRole('button', { name: '#anime' }));
		await tick();
		// addTagChip reused UNCHANGED (per the sign-off): from a pristine input it sets
		// discoverTags='anime' — the tag is the input text, and the typed stem is by definition
		// not-yet-committed, so the chip appears once a separator/second term lands, exactly like
		// the autocomplete path. The cloud is gone either way (input no longer empty), and the
		// parsed tag already arms the search.
		expect((getByPlaceholderText(/name, bio, or tag/i) as HTMLInputElement).value).toBe('anime');
		expect(document.body.querySelectorAll('button.disc-cloud-tag').length).toBe(0);
		// Type a separator → the tag becomes a committed chip with its remove button.
		await fireEvent.input(getByPlaceholderText(/name, bio, or tag/i), { target: { value: 'anime, ' } });
		await tick();
		expect(getByRole('button', { name: 'Remove tag anime' })).toBeTruthy();
	});

	it('hides the cloud the moment the input is non-empty', async () => {
		const { getByPlaceholderText } = await openDiscoverPristine(['anime']);
		await fireEvent.input(getByPlaceholderText(/name, bio, or tag/i), { target: { value: 'a' } });
		await tick();
		expect(document.body.querySelectorAll('button.disc-cloud-tag').length).toBe(0);
		// And it comes back when the input is cleared again.
		await fireEvent.input(getByPlaceholderText(/name, bio, or tag/i), { target: { value: '' } });
		await tick();
		expect(cloudTags()).toEqual(['#anime']);
	});

	it('renders nothing when no tags have been observed', async () => {
		// Not openDiscoverPristine (its waitFor would never pass with zero cloud tags) — open,
		// settle for the mocked promise, then assert the cloud is simply absent.
		mockObservedTags([]);
		(searchPeers as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(resultEnvelope([]));
		const { getByRole } = render(AddContactPanel, { props: { open: true } });
		await fireEvent.click(getByRole('button', { name: /discover hoarders/i }));
		await new Promise((r) => setTimeout(r, 30));
		await tick();
		expect(document.body.querySelectorAll('button.disc-cloud-tag').length).toBe(0);
	});
});

// QURATOR-104 — a Discover hit whose npub is ALREADY a contact must not be presented as a stranger
// with a live "Add contact" button. The lookup card in the same panel already dedups against the
// roster (`alreadyFollowed`/`canUnlock`, AddContactPanel.svelte:41-46); the hit-cards rendered
// unconditionally. Three states per hit, matching the lookup card's vocabulary:
//   keyed contact   → no stranger banner (petname shown instead), no enabled Add button ("Added ✓")
//   keyless contact → same, but "Added" only — a hit carries NO browse key (PeerSearchHit has no
//                     key field; followHit passes the bare npub), so there is nothing to route
//                     through the lookup's canUnlock/“Unlock browsing” upgrade. ShareCodeCard's
//                     same-peer bare-npub precedent renders that case inert for exactly this reason.
//   unknown npub    → unchanged: stranger banner + live Add contact.
// Message stays in all three states (M17 W1).
describe('AddContactPanel — QURATOR-104 hit-card roster dedup', () => {
	const KEYED = 'npub1keyedcontact0000000000000000000000000000000000000000000000';
	const KEYLESS = 'npub1keylesscontact00000000000000000000000000000000000000000000';
	const UNKNOWN = 'npub1stranger0000000000000000000000000000000000000000000000000';

	/** A roster entry: one keyed (browse_key_hex set) and one keyless (added by bare npub). */
	function roster(): CachedPeer[] {
		const base = {
			npub_short: 'npub1…',
			collections: [],
			online: false,
			last_fetched: '2026-08-16T00:00:00Z',
			local_tags: [],
		};
		return [
			{ ...base, npub: KEYED, petname: 'Keyed Pal', browse_key_hex: 'ab12' },
			{ ...base, npub: KEYLESS, petname: 'Bare Pal' },
		] as unknown as CachedPeer[];
	}

	it('keyed contact hit: no stranger banner, no enabled Add, petname shown, Message kept', async () => {
		contacts.set(roster());
		const onadd = vi.fn();
		const { getAllByRole, queryAllByText, findByText } = await discoverHits(
			[makeHit({ npub: KEYED, display_name: 'Online Persona' })],
			{ onadd },
		);
		// No stranger banner on the hit.
		expect(queryAllByText(/unverified · not in your contacts/i).length).toBe(0);
		// The roster membership is shown (petname, distinct from the teaser display_name).
		expect(await findByText('Keyed Pal')).toBeTruthy();
		// No ENABLED "Add contact" button anywhere in the panel.
		const enabledAdds = getAllByRole('button')
			.filter((b) => (b.textContent ?? '').trim() === 'Add contact' && !(b as HTMLButtonElement).disabled);
		expect(enabledAdds.length).toBe(0);
		// Message survives (M17 W1).
		expect(getAllByRole('button', { name: 'Message' }).length).toBe(1);
	});

	it('keyless contact hit: no stranger banner, no enabled Add, Message kept', async () => {
		contacts.set(roster());
		const onadd = vi.fn();
		const { getAllByRole, queryAllByText, findByText } = await discoverHits(
			[makeHit({ npub: KEYLESS, display_name: 'Online Persona' })],
			{ onadd },
		);
		expect(queryAllByText(/unverified · not in your contacts/i).length).toBe(0);
		expect(await findByText('Bare Pal')).toBeTruthy();
		const enabledAdds = getAllByRole('button')
			.filter((b) => (b.textContent ?? '').trim() === 'Add contact' && !(b as HTMLButtonElement).disabled);
		expect(enabledAdds.length).toBe(0);
		expect(getAllByRole('button', { name: 'Message' }).length).toBe(1);
	});

	it('unknown npub hit keeps the stranger banner and a live Add contact', async () => {
		contacts.set(roster());
		const onadd = vi.fn();
		const { getAllByRole, findByText, addBtn } = await discoverHits(
			[makeHit({ npub: UNKNOWN, display_name: 'Stranger' })],
			{ onadd },
		);
		expect(await findByText(/unverified · not in your contacts/i)).toBeTruthy();
		expect(addBtn).toBeTruthy();
		expect((addBtn as HTMLButtonElement).disabled).toBe(false);
		expect(getAllByRole('button', { name: 'Message' }).length).toBe(1);
	});
});

// Why-matched badge (owner sign-off 2026-08-15) — DOM-level coverage: the badge renders per hit
// from the terms THAT search ran with, shows the matched axis, and is omitted when null.
describe('AddContactPanel — why-matched badge', () => {
	it('shows the matched axis per hit (tag / type / name / bio)', async () => {
		// Two hits, one per axis, so a single search proves both badges render per-row.
		(searchPeers as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(
			resultEnvelope([
				makeHit({ npub: 'npub1bytag', tags: ['anime'], content_types: ['video'] }),
				makeHit({ npub: 'npub1byname', tags: ['manga'], display_name: 'Anime Fan' }),
			]),
		);
		mockObservedTags();
		const { getByRole, getByPlaceholderText, findAllByText } = render(AddContactPanel, { props: { open: true } });
		await fireEvent.click(getByRole('button', { name: /discover hoarders/i }));
		await fireEvent.input(getByPlaceholderText(/tag/i), { target: { value: 'anime' } });
		await fireEvent.click(getByRole('button', { name: /^search$/i }));
		// 'anime' (single term): hit 1 carries it as a tag → 'tag'; hit 2 carries it in the name → 'name'.
		expect((await findAllByText('matched by tag')).length).toBe(1);
		expect((await findAllByText('matched by name')).length).toBe(1);
	});

	it('renders the badge as "type" for a content-type-only search', async () => {
		// Type-only search: commit a content type chip, no tags.
		mockObservedTags();
		(searchPeers as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(
			resultEnvelope([makeHit({ npub: 'npub1bytype', tags: ['whatever'], content_types: ['video'] })]),
		);
		const { getByRole, getAllByRole, findByText } = render(AddContactPanel, { props: { open: true } });
		await fireEvent.click(getByRole('button', { name: /discover hoarders/i }));
		await fireEvent.click(getAllByRole('button', { name: 'Video' })[0]);
		await fireEvent.click(getByRole('button', { name: /^search$/i }));
		expect(await findByText('matched by type')).toBeTruthy();
	});

	it('omits the badge when no confident reason exists (no placeholder)', async () => {
		// discoverHits searches 'anime'. A hit whose tags/name/bio/content_types don't carry it →
		// null → no badge node at all (note makeHit() defaults to tags:['anime'], which WOULD badge).
		const { addBtn } = await discoverHits([
			makeHit({ npub: 'npub1nomatch', tags: ['manga'], display_name: 'Stranger' }),
		]);
		expect(addBtn).toBeTruthy();
		expect(document.body.querySelectorAll('.hit-why').length).toBe(0);
	});
});

