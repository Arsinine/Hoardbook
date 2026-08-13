// QURATOR-81 — layout-level source-scan: the window chrome lives ONCE in +layout.svelte (not
// per-route), so every route — including Browse and Chat, which have no .topbar — gets the controls.
// Source-text assertions, the repo's established pattern for route-page guards (see topics-w9.test.ts).
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const layoutSrc = () =>
	readFileSync(new URL('./+layout.svelte', import.meta.url), 'utf8');

describe('QURATOR-81 — window chrome in the layout shell', () => {
	it('WindowControls is imported and mounted once in the layout, not per-route', () => {
		const s = layoutSrc();
		expect(s).toContain("from '$lib/components/WindowControls.svelte'");
		expect(s).toContain('<WindowControls');
		// Exactly ONE mount site (not duplicated).
		const mounts = s.match(/<WindowControls\b/g);
		expect(mounts).not.toBeNull();
		expect(mounts!.length).toBe(1);
	});

	it('the controls live inside .main so they cover every route', () => {
		const s = layoutSrc();
		const mainOpen = s.indexOf('<div class="main">');
		expect(mainOpen).toBeGreaterThan(-1);
		const mainBlock = s.slice(mainOpen, s.indexOf('</div>\n\t</div>', mainOpen));
		expect(mainBlock).toContain('<WindowControls');
	});

	it('a drag region is present for window dragging', () => {
		const s = layoutSrc();
		// data-tauri-drag-region is Tauri v2's drag trigger.
		expect(s).toContain('data-tauri-drag-region');
	});

	// ⚠ The assertion above is WEAK BY CONSTRUCTION and the owner paid for it. It scans the whole
	// layout file for the attribute, which was satisfied by an 8px strip — and on Windows the top few
	// pixels are also the resize hotspot, so the real grab area was near zero and the owner reported
	// "there's no drag". A whole-file scan cannot measure a drag target. jsdom computes no layout, so
	// nothing here can either; what these tests CAN pin is that the attribute sits on the full-width
	// topbar element rather than only on a sliver. That is the structural half of the fix.
	it.each(['contacts', 'settings', 'topics'])(
		'the %s topbar element itself carries the drag attribute, not just the file',
		(route) => {
			const src = readFileSync(new URL(`./${route}/+page.svelte`, import.meta.url), 'utf8');
			// Slice the OPENING TAG, not the file: a match anywhere else (a comment, the 8px strip)
			// must not satisfy this. Same lesson as the W4 missing-import that passed a file-wide scan.
			const tag = src.match(/<div class="topbar"[^>]*>/);
			expect(tag, `${route} has no .topbar opening tag`).not.toBeNull();
			expect(tag![0]).toContain('data-tauri-drag-region');
		},
	);

	it('the layout drag strip is documented as a fallback, not the primary handle', () => {
		// Browse and Chat have no .topbar, so the thin strip is still all they have. Recorded here
		// so the next reader knows the coverage is uneven rather than assuming it is uniform.
		const s = layoutSrc();
		expect(s).toContain('win-drag-top');
	});

	it('route topbars get right-padding so actions never slide under the controls', () => {
		// The :global(.topbar) padding rule is the one-site fix for the Contacts/Browse drift pair:
		// instead of editing each route's topbar, a single rule in the layout reserves the space.
		const s = layoutSrc();
		expect(s).toMatch(/:global\(\.topbar\)\s*\{[^}]*padding-right/);
	});
});
