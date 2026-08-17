// QURATOR-98 — Browse's drag-group namer was the app's one hand-rolled dialog: a fixed-position
// `.dg-panel` + `.dg-backdrop` pair living at `--z-menu` (BELOW `--z-modal`), with no focus trap
// beyond the input's own Esc handler and no focus-restore coordination with the shared shell.
// Every other dialog in the app is the shared `Modal` component (M15 W2), which owns the
// backdrop, the accessible dialog card, Escape, the Tab trap, and focus restore.
//
// This file pins the fix from the SOURCE side: the namer renders through `<Modal`, the hand-rolled
// `.dg-backdrop`/`.dg-panel` shell is gone, and the input carries `hb-input dg-input` parity with
// Contacts' twin (QURATOR-52 §5: the input contract is the global `.hb-input`, only layout local).
// The behavioural half (Modal's trap/restore/Esc) is already pinned by Modal.test.ts (13 tests),
// so routing the namer through Modal inherits them; the drag-group commit logic itself is pinned
// by browse-m22-drag.test.ts and must not change.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const src = readFileSync(fileURLToPath(new URL('./+page.svelte', import.meta.url)), 'utf8');

/** Strip Svelte `<!-- -->` comments so absence assertions don't collide with documentation. */
function stripComments(s: string): string {
	return s.replace(/<!--[\s\S]*?-->/g, '');
}

/** The namer block: from the `{#if dragPopoverFor}` to its closing `{/if}`. */
function namerBlock(s: string): string {
	const start = s.indexOf('{#if dragPopoverFor}');
	if (start === -1) return '';
	// First '{/if}' at column 0 after the block opens — the block's own closer.
	const m = /\n\{\/if\}/.exec(s.slice(start));
	return m ? s.slice(start, start + m.index + m[0].length) : '';
}

describe('QURATOR-98 — Browse\'s drag-group namer uses the shared Modal shell', () => {
	it('the namer renders through the shared Modal component', () => {
		const block = namerBlock(stripComments(src));
		expect(block).not.toBe('');
		expect(block).toMatch(/<Modal\b/);
		expect(block).toMatch(/onclose=\{closeDragPopover\}/);
	});

	it('the hand-rolled shell is gone: no .dg-backdrop, no .dg-panel', () => {
		const block = namerBlock(stripComments(src));
		expect(block).not.toMatch(/dg-backdrop/);
		expect(block).not.toMatch(/class="dg-panel"/);
		// And their styles are gone from the stylesheet too, not just the markup.
		const styles = stripComments(src).slice(src.indexOf('<style>'));
		expect(styles).not.toMatch(/\.dg-backdrop/);
		expect(styles).not.toMatch(/\.dg-panel/);
	});

	it('the input has hb-input parity with Contacts\' twin', () => {
		const block = namerBlock(stripComments(src));
		expect(block).toMatch(/class="hb-input dg-input"/);
	});

	it('the namer content the M22 suites pin survives the shell swap', () => {
		const block = namerBlock(stripComments(src));
		// browse-m22-drag.test.ts pins dg-count; placeholder + Enter/Esc wiring stay live.
		expect(block).toMatch(/dg-count/);
		expect(block).toMatch(/placeholder="Name this group"/);
		expect(block).toMatch(/bind:this=\{dragNameEl\}/);
		expect(block).toMatch(/onkeydown=\{onDragNameKey\}/);
	});
});
