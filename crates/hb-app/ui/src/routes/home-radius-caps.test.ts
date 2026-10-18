// QURATOR-382 — home/+page.svelte sites that use `var(--r-control, …)` but are NOT single-line
// controls went stadium-shaped on the five 999px pill themes (vault, clinic-teal, pale-pulse,
// sky-slate, violet-drive): a multi-line error box, the npub display row, a read-only field box,
// a flex-wrap tag container and the language-suggestion dropdown. Fix is the app.css
// `.hb-textarea` precedent:
//
//     border-radius: min(var(--r-control, <site fallback>), var(--r-card, 10px));
//
// ⚠ P-13 — this is a DECLARED-CSS check: jsdom computes no layout, so nothing here proves how a
// box RENDERS; it pins the exact declaration text in each fixed rule block, comments stripped
// first (P-12) and anchored on the ` {` brace so `.foo {` never matches `.foo:hover`. Genuine
// single-line controls and chips on this page (.social-icon-btn, .lang-tag, .willing-btn,
// .ct-badge, .btn-add) stay pills by design (owner: pill controls) and are deliberately NOT
// pinned here.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const src = (file: string) => readFileSync(new URL(file, import.meta.url), 'utf8');

/** Strip block comments (P-12: a guard must never count prose), then slice the rule block that
 *  starts at `needle` (must include the brace, so `.tag-wrap {` never matches a sibling selector)
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

// MUTATION (P-10): temporarily restored the bare `border-radius: var(--r-control, 6px);` in
// `.id-error-detail` and reran this file — the .id-error-detail test REDD (1 failed | 2 passed,
// exit 1), restore landed, rerun GREEN (3 passed, exit 0).


describe('QURATOR-382 — home page non-control radius caps at --r-card (declared CSS, not rendered)', () => {
	it('.id-error-detail (multi-line error box, 6px fallback) caps at the card radius', () => {
		expect(radiusDecl(ruleBlock(src('./+page.svelte'), '.id-error-detail {'))).toBe(CAP('6px'));
	});

	it('.ob-hbid-row, .field-readonly, .tag-wrap and .lang-suggestions (7px fallbacks) cap at the card radius', () => {
		const s = src('./+page.svelte');
		expect(radiusDecl(ruleBlock(s, '.ob-hbid-row {'))).toBe(CAP('7px'));
		expect(radiusDecl(ruleBlock(s, '.field-readonly {'))).toBe(CAP('7px'));
		expect(radiusDecl(ruleBlock(s, '.tag-wrap {'))).toBe(CAP('7px'));
		expect(radiusDecl(ruleBlock(s, '.lang-suggestions {'))).toBe(CAP('7px'));
	});

	it('genuine controls keep their bare pill radii (untouched by the cap)', () => {
		const s = src('./+page.svelte');
		expect(radiusDecl(ruleBlock(s, '.social-icon-btn {'))).toBe('border-radius: var(--r-control, 6px)');
		expect(radiusDecl(ruleBlock(s, '.lang-tag {'))).toBe('border-radius: var(--r-chip, 4px)');
		expect(radiusDecl(ruleBlock(s, '.ct-badge {'))).toBe('border-radius: var(--r-chip, 4px)');
	});
});
