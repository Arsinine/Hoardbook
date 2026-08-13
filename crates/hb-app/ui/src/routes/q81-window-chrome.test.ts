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

	it('route topbars get right-padding so actions never slide under the controls', () => {
		// The :global(.topbar) padding rule is the one-site fix for the Contacts/Browse drift pair:
		// instead of editing each route's topbar, a single rule in the layout reserves the space.
		const s = layoutSrc();
		expect(s).toMatch(/:global\(\.topbar\)\s*\{[^}]*padding-right/);
	});
});
