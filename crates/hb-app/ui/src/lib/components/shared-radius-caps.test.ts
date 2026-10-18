// QURATOR-382 — five themes (vault, clinic-teal, pale-pulse, sky-slate, violet-drive) set
// `--r-control: 999px; --r-chip: 999px;`, so any element that borrows a CONTROL radius while NOT
// being a single-line control renders as a stadium: the sidebar nav row, a multi-line key-storage
// warning box, the 280px scrolling folder-tree box, full-width list rows and the popover's
// "new group" row. The fix is the app.css `.hb-textarea` / QURATOR-381 precedent (c84d4678):
//
//     border-radius: min(var(--r-control, <site fallback>), var(--r-card, 10px));
//
// ⚠ P-13 — this is a DECLARED-CSS check: jsdom computes no layout, so nothing here proves how a
// site RENDERS; it pins the exact declaration text in each fixed rule block, with comments
// stripped first (P-12) and anchored on the exact `border-radius:` declaration. The ruleBlock
// needle includes the ` {` brace so `.foo {` never matches `.foo:hover`. Genuine single-line
// controls stay pills by design (owner: pill controls) — layout's .toast-action, the modal's
// .ct-toggle and .chip are deliberately NOT pinned here. .gmp-row was reported uncapped
// (QURATOR-381 did not reach it); lane D capped and pinned it in the second describe below.
//
// MUTATION (P-10): each of the six pinned sites (one per file) was temporarily reverted to the
// bare `var(--r-control, …)` declaration and this file re-run — all six tests RED (exit=1,
// 6 failed / 6, laneB-vt-red.log). Restored and re-run — GREEN (exit=0, 6 passed / 6,
// laneB-vt-green2.log). Seen red 2026-10-18.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const src = (file: string) => readFileSync(new URL(file, import.meta.url), 'utf8');

/** Strip block comments (P-12: a guard must never count prose), then slice the rule block that
 *  starts at `needle` (must include the brace, so `.gmp-new {` never matches `.gmp-new:hover`)
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

describe('QURATOR-382 — non-control radius caps at --r-card (declared CSS, not rendered)', () => {
	it('layout: .nav-item (full-width sidebar nav row) caps at the card radius', () => {
		expect(radiusDecl(ruleBlock(src('../../routes/+layout.svelte'), '.nav-item {'))).toBe(CAP('7px'));
	});

	it('settings: .key-storage-warn (multi-line warning box) caps at the card radius', () => {
		expect(radiusDecl(ruleBlock(src('../../routes/settings/+page.svelte'), '.key-storage-warn {'))).toBe(CAP('7px'));
	});

	it('AddCollectionModal: .tree-box (280px scrolling folder tree) caps at the card radius', () => {
		expect(radiusDecl(ruleBlock(src('./AddCollectionModal.svelte'), '.tree-box {'))).toBe(CAP('7px'));
	});

	it('ContactPicker: .contact-row (full-width list row) caps at the card radius', () => {
		expect(radiusDecl(ruleBlock(src('./ContactPicker.svelte'), '.contact-row {'))).toBe(CAP('7px'));
	});

	it('PersonRow: .person-row (full-width list row) caps at the card radius', () => {
		expect(radiusDecl(ruleBlock(src('./PersonRow.svelte'), '.person-row {'))).toBe(CAP('5px'));
	});

	it('GroupMembershipPopover: .gmp-new (new-group row) caps at the card radius', () => {
		expect(radiusDecl(ruleBlock(src('./GroupMembershipPopover.svelte'), '.gmp-new {'))).toBe(CAP('5px'));
	});
});

// QURATOR-382 lane D — three residual sites. .gmp-row is the popover's full-width row (never
// capped); .tag-wrap / .lang-wrap declared NO border-radius of their own and INHERITED .hb-input's
// bare `var(--r-control, 7px)` from app.css, so on 999px themes they went stadium once their
// flex-wrapped content grew past one line. Both now declare the capped radius explicitly.
//
// MUTATION (P-10): one site per file was reverted (…/laneD-vt-red.log): .gmp-row back to bare
// `var(--r-control, 5px)`, and the ADDED declaration deleted from .tag-wrap and .lang-wrap (for an
// ADD, the mutation is the deletion — radiusDecl's truthy check then reds). All three RED (exit=1,
// 3 failed / 9). Restored — GREEN (exit=0, 9 passed / 9, laneD-vt-green.log). Seen red 2026-10-18.
describe('QURATOR-382 lane D — residual non-control sites', () => {
	it('GroupMembershipPopover: .gmp-row (full-width popover row) caps at the card radius', () => {
		expect(radiusDecl(ruleBlock(src('./GroupMembershipPopover.svelte'), '.gmp-row {'))).toBe(CAP('5px'));
	});

	it('CollectionTagsEditor: .tag-wrap (flex-wrap multi-line tag box) declares the capped radius it used to inherit bare', () => {
		expect(radiusDecl(ruleBlock(src('./CollectionTagsEditor.svelte'), '.tag-wrap {'))).toBe(CAP('7px'));
	});

	it('AddCollectionModal: .lang-wrap (flex-wrap multi-line language box) declares the capped radius it used to inherit bare', () => {
		expect(radiusDecl(ruleBlock(src('./AddCollectionModal.svelte'), '.lang-wrap {'))).toBe(CAP('7px'));
	});
});
