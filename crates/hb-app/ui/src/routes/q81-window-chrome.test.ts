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
	it.each(['contacts', 'settings', 'topics', 'home'])(
		'the %s topbar element itself carries the drag attribute, not just the file',
		(route) => {
			// 'home' is the root route — its +page.svelte sits next to this test file (routes/), not
			// under a subdirectory. It was the one .topbar missing the attribute (owner report
			// 2026-08-18), fixed alongside Browse/Chat below.
			const file = route === 'home' ? './+page.svelte' : `./${route}/+page.svelte`;
			const src = readFileSync(new URL(file, import.meta.url), 'utf8');
			// Slice the OPENING TAG, not the file: a match anywhere else (a comment, the 8px strip)
			// must not satisfy this. Same lesson as the W4 missing-import that passed a file-wide scan.
			const tag = src.match(/<div class="topbar"[^>]*>/);
			expect(tag, `${route} has no .topbar opening tag`).not.toBeNull();
			expect(tag![0]).toContain('data-tauri-drag-region');
		},
	);

	it('Browse panel-top (the one unconditional header it has) carries the drag attribute', () => {
		// Browse's right panel is peer-selection-dependent and has no unconditional header, so full
		// parity with the .topbar routes isn't possible without a layout change (owner-gated, see
		// QURATOR-81). panel-top in the left sidebar is the pragmatic fix: always present, clear of
		// the window controls (no padding-right needed — they're anchored top-right of .main).
		const src = readFileSync(new URL('./browse/+page.svelte', import.meta.url), 'utf8');
		const tag = src.match(/<div class="panel-top"[^>]*>/);
		expect(tag, 'browse has no .panel-top opening tag').not.toBeNull();
		expect(tag![0]).toContain('data-tauri-drag-region');
	});

	it('every Chat pane-header carries the drag attribute (topic channel, requests, opened request, conversation)', () => {
		// All four pane-header instances share one CSS rule; the fix is applied to all four div tags
		// so drag coverage doesn't depend on which right-pane view is currently showing.
		const src = readFileSync(new URL('./chat/+page.svelte', import.meta.url), 'utf8');
		const tags = src.match(/<div class="pane-header"[^>]*>/g);
		expect(tags, 'chat has no .pane-header opening tags').not.toBeNull();
		expect(tags!.length).toBe(4);
		for (const tag of tags!) {
			expect(tag).toContain('data-tauri-drag-region');
		}
	});

	it('the layout drag strip is documented as a fallback, still the only cover for Browse\'s right panel', () => {
		// Browse's right (collection-detail) panel still has no unconditional header — the thin
		// strip is what it has there. Recorded so the next reader knows that gap is deliberate, not
		// an oversight, unlike the Home/Chat gaps this ticket closed.
		const s = layoutSrc();
		expect(s).toContain('win-drag-top');
	});

	it('route topbars get right-padding so actions never slide under the controls', () => {
		// The :global(.topbar) padding rule is the one-site fix for the Contacts/Browse drift pair:
		// instead of editing each route's topbar, a single rule in the layout reserves the space.
		const s = layoutSrc();
		expect(s).toMatch(/:global\(\.topbar\)\s*\{[^}]*padding-right/);
	});

	it('Chat pane-header reserves the same right-padding, fixing the View-profile overlap', () => {
		// pane-header isn't .topbar, so the global :global(.topbar) rule above doesn't reach it — it
		// needs its own padding-right. Without this, "View profile" (and the topic-channel/requests
		// header actions) render under the absolute-positioned window controls.
		const src = readFileSync(new URL('./chat/+page.svelte', import.meta.url), 'utf8');
		expect(src).toMatch(/\.pane-header\s*\{[^}]*padding-right:\s*120px/);
	});
});
