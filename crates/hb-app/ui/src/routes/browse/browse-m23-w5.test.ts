// M23 W5 — the Browse People ROW FORM (devtest v0.13.0 item 6).
//
// ⚠ WHY THIS FILE EXISTS AT ALL. The row was compacted to the approved mock — 24px avatar, name,
// bare collection count right-aligned in a trailing slot — and the existing browse suites stayed
// green through a deliberate mutation that swapped the count and the drag-outcome label (86/86
// green on broken code). Those suites pin the M22 drag/selection/roving-tabindex ATTRIBUTES, which
// this change left byte-identical, so they are structurally blind to the row's shape. A green run
// of them says nothing about this workstream. This file is the discriminator.
//
// Source-scan idiom (see browse-w5b, browse-w10, ask-access-w2): the route's onMount fan-out +
// $app/navigation goto make a full mount heavier than the wiring checks warrant.
//
// ⚠ HONEST LIMIT, stated up front: these are SOURCE assertions. jsdom computes no layout, so
// nothing here proves the row RENDERS as one line, that the avatar is visually 24px, or that the
// count sits flush right. What it does prove is that the source still SAYS those things — i.e. it
// catches a regression of the decision, not a failure of the browser. Visual confirmation needs a
// real screenshot and is deliberately out of scope.

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const browseSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

/** The People-panel markup slice only. The right-hand browser panel also renders collection counts
 *  and prose, so an unscoped absence assertion would match that instead and pass vacuously. */
function peoplePanel(s: string): string {
	const start = s.indexOf('class="contact-list"');
	const end = s.indexOf('<!-- Right: browser');
	expect(start).toBeGreaterThan(-1);
	expect(end).toBeGreaterThan(start);
	return s.slice(start, end);
}

/** Strip HTML and JS comments before any ABSENCE assertion. The page documents its own rulings in
 *  prose (e.g. the v0.12.1 note about the inline text badge), so a raw scan reds on the very
 *  comment that explains the rule — the sentinel-collision trap CLAUDE.md §9 names. */
function stripComments(s: string): string {
	return s.replace(/<!--[\s\S]*?-->/g, '').replace(/\/\/[^\n]*/g, '');
}

describe('M23 W5 — the People row is a single compact line', () => {
	it('the row avatar is 24px, matching the mock (was 28)', () => {
		const panel = peoplePanel(browseSrc());
		expect(panel).toMatch(/<Avatar \{letter\} size=\{24\}/);
	});

	it('the two-line .contact-info column is gone — name and count are siblings in the flex row', () => {
		const panel = stripComments(peoplePanel(browseSrc()));
		expect(panel).not.toMatch(/class="contact-info"/);
	});

	it('the trailing slot shows the BARE count, not the words "N collections"', () => {
		const panel = stripComments(peoplePanel(browseSrc()));
		// The mock's trailing value is a number: 7, 2, 12 — the pluralised prose belongs to the
		// right-hand panel, not to a compact row.
		expect(panel).toMatch(/<span class="contact-meta">\{peer\.collections\.length\}<\/span>/);
		expect(panel).not.toMatch(/collection\{peer\.collections\.length !== 1/);
	});

	// ── THE DISCRIMINATOR ────────────────────────────────────────────────────────────────────
	// This is the assertion the pre-existing suites could not make. Swapping the branches (so the
	// drag-outcome always renders and the count is hidden) left browse-m22-drag + browse-w5b at
	// 86/86 green. It reds here.
	it('the count is the DEFAULT and the drag-outcome only replaces it during a drag on this row', () => {
		const panel = stripComments(peoplePanel(browseSrc()));
		// One conditional, two alternatives, in this order: outcome in the {#if}, count in the {:else}.
		// Inverting them means every row permanently advertises a drag that is not happening and the
		// collection count disappears from the panel entirely.
		expect(panel).toMatch(
			/\{#if dragSourceNpub !== null[^}]*\}\s*<span class="drag-outcome-browse">[\s\S]*?\{:else\}\s*<span class="contact-meta">\{peer\.collections\.length\}<\/span>\s*\{\/if\}/,
		);
	});

	it('the lock stays on the AVATAR and never takes the trailing slot (owner ruling 2026-08-11)', () => {
		const panel = peoplePanel(browseSrc());
		// The mock drew 🔒 in the trailing slot; the owner ruled it stays as the avatar overlay,
		// preserving the v0.12.1 #3 devtest fix ("the inline text badge is gone"). So .access-lock
		// must sit inside .avatar-wrap, and the trailing slot must remain the count.
		expect(panel).toMatch(
			/<div class="avatar-wrap">[\s\S]*?<span class="access-lock"[\s\S]*?<\/div>/,
		);
	});
});

describe('M23 W5 — the CSS that makes it one line and right-aligns the count', () => {
	it('.contact-meta is pushed to the right edge and does not shrink', () => {
		const s = browseSrc();
		expect(s).toMatch(/\.contact-meta \{[^}]*margin-left: auto;[^}]*\}/);
		expect(s).toMatch(/\.contact-meta \{[^}]*flex-shrink: 0;[^}]*\}/);
	});

	it('.contact-name is no longer a block (a block forces the second line back)', () => {
		const s = browseSrc();
		const rule = s.match(/\n\t\.contact-name \{[^}]*\}/)?.[0] ?? '';
		expect(rule).not.toBe('');
		expect(rule).not.toMatch(/display: block/);
		// It must still be able to shrink, or a long petname pushes the count off the row.
		expect(rule).toMatch(/min-width: 0/);
		expect(rule).toMatch(/text-overflow: ellipsis/);
	});

	it('the drag-outcome shares the trailing slot rather than stacking a third line', () => {
		const s = browseSrc();
		const rule = s.match(/\.drag-outcome-browse \{[^}]*\}/)?.[0] ?? '';
		expect(rule).not.toBe('');
		expect(rule).toMatch(/margin-left: auto/);
		expect(rule).not.toMatch(/display: block/);
	});

	it('the selection ring stays symmetric (M23 W1 — no offset inset shadow anywhere)', () => {
		// Guards the sibling fix from regressing while this row is being restyled: the Contacts/Browse
		// drift pair is this repo's standing fault shape.
		expect(browseSrc()).not.toMatch(/inset \d+px 0 0/);
	});
});
