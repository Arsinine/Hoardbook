// QURATOR-381 — Topics list rows went stadium-shaped on themes whose `--r-control` is 999px
// (vault, clinic-teal, violet-drive, sky-slate, pale-pulse): the bare `var(--r-control, …)` was a
// CONTROL radius on elements that are not controls — full-width LIST ROWS, their hover fills and
// the roster strip. The fix is the app.css `.hb-textarea` precedent (owner devtest v0.25.0, "the
// bio input field is excessively curved"):
//
//     border-radius: min(var(--r-control, <site fallback>), var(--r-card, 10px));
//
// ⚠ P-13 — this is a DECLARED-CSS check: jsdom computes no layout, so nothing here proves how a
// row RENDERS; it pins the declaration text in each fixed rule block, with comments stripped
// first (P-12) and anchored on the exact `border-radius:` declaration (a coarser "contains min("
// would be satisfiable by an unrelated declaration). Rendered-shape proof is the Playwright
// before/after shots on `vault`. Genuine single-line buttons and chips stay pills by design
// (owner: pill controls) — TopicJoinConsent's Cancel/Join, chat's .btn-send, contacts'
// .view-toggle / .row-menu-btn / .menu-item / .show-more are deliberately NOT pinned here.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const src = (file: string) => readFileSync(new URL(file, import.meta.url), 'utf8');

/** Strip block comments (P-12: a guard must never count prose), then slice the rule block that
 *  starts at `needle` (must include the brace, so `.topic-row {` never matches `.topic-row:hover`)
 *  through its closing brace — the fixed rules contain no nested braces. */
function ruleBlock(source: string, needle: string): string {
	const start = source.indexOf(needle);
	expect(start, `${needle} exists`).toBeGreaterThan(-1);
	const body = source.slice(start, source.indexOf('}', start));
	return body.replace(/\/\*[\s\S]*?\*\//g, '');
}

/** The one `border-radius:` declaration in a stripped block (semicolons never appear inside min()). */
function radiusDecl(block: string): string {
	const decl = block.split(';').map((s) => s.trim()).find((s) => s.startsWith('border-radius:'));
	expect(decl, 'the block declares a border-radius').toBeTruthy();
	return decl!;
}

const CAP = (fallback: string) => `border-radius: min(var(--r-control, ${fallback}), var(--r-card, 10px))`;

/** QURATOR-382 chip variant: same cap shape for sites whose bare token is --r-chip. */
const CHIP_CAP = (fallback: string) => `border-radius: min(var(--r-chip, ${fallback}), var(--r-card, 10px))`;

describe('QURATOR-381 — list-row radius caps at --r-card (declared CSS, not rendered)', () => {
	it('topics: .topic-row, .unjoined:hover and .roster-row cap at the card radius', () => {
		const s = src('./+page.svelte');
		expect(radiusDecl(ruleBlock(s, '.topic-row {'))).toBe(CAP('7px'));
		expect(radiusDecl(ruleBlock(s, '.unjoined:hover {'))).toBe(CAP('7px'));
		expect(radiusDecl(ruleBlock(s, '.roster-row {'))).toBe(CAP('5px'));
	});

	it('chat: .convo-item and .request-row cap at the card radius', () => {
		const s = src('../chat/+page.svelte');
		expect(radiusDecl(ruleBlock(s, '.convo-item {'))).toBe(CAP('7px'));
		expect(radiusDecl(ruleBlock(s, '.request-row {'))).toBe(CAP('7px'));
	});

	it('contacts: .cpop-row caps at the card radius', () => {
		const s = src('../contacts/+page.svelte');
		expect(radiusDecl(ruleBlock(s, '.cpop-row {'))).toBe(CAP('6px'));
	});
});

// QURATOR-382 — five MORE non-control sites went stadium/circle on the 999px pill themes
// (vault, clinic-teal, pale-pulse, sky-slate, violet-drive): a full-width sticky header strip,
// a full-width list row, a multi-line bio box, and two square icon tiles. Same fix and same
// limits as QURATOR-381 above: DECLARED-CSS pins only (P-13 — jsdom computes no layout, so
// nothing here proves how anything RENDERS; rendered-shape proof is the Playwright before/after
// shots on `vault`). Comments stripped first (P-12); needle includes the ` {` brace so
// `.contact-row {` never matches the `.contact-row { transition: none; }` media-query block.
// MUTATION (P-10): each of the five declarations below was reverted in place to its bare
// `var(--r-control, …)` / `var(--r-chip, …)` form (all five at once) and this file was re-run:
// exactly the five QURATOR-382 tests RED and the QURATOR-381 tests stayed green. Seen red.
// Restored and re-read: all five capped declarations back in place, suite green.
describe('QURATOR-382 — non-control radius caps at --r-card (declared CSS, not rendered)', () => {
	it('contacts: the glass-float .section-header strip caps at the card radius', () => {
		const s = src('../contacts/+page.svelte');
		expect(radiusDecl(ruleBlock(s, ':global(:root[data-glass-float]) .section-header {'))).toBe(CHIP_CAP('4px'));
	});

	it('browse: .contact-row caps at the card radius', () => {
		const s = src('../browse/+page.svelte');
		expect(radiusDecl(ruleBlock(s, '.contact-row {'))).toBe(CAP('5px'));
	});

	it('topics: .roster-bio caps at the card radius', () => {
		const s = src('./+page.svelte');
		expect(radiusDecl(ruleBlock(s, '.roster-bio {'))).toBe(CHIP_CAP('4px'));
	});

	it('CollectionRow: .row-icon caps at the card radius', () => {
		const s = src('../../lib/components/CollectionRow.svelte');
		expect(radiusDecl(ruleBlock(s, '.row-icon {'))).toBe(CAP('7px'));
	});

	it('CollectionPanel: .folder-icon caps at the card radius', () => {
		const s = src('../../lib/components/CollectionPanel.svelte');
		expect(radiusDecl(ruleBlock(s, '.folder-icon {'))).toBe(CAP('7px'));
	});
});
