// QURATOR-81 — layout-level source-scan: the window chrome lives ONCE in +layout.svelte (not
// per-route), so every route gets the controls. (When this was written Browse and Chat had no
// .topbar at all; devtest 2026-08-26 item 5 gave them one — see page-bar-item5.test.ts, which
// MOUNTS both pages rather than scanning them.)
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
	it.each(['contacts', 'settings', 'topics', 'home', 'browse', 'chat'])(
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

	it('Browse panel-top keeps its drag attribute alongside the topbar added in item 5', () => {
		// Was "the one unconditional header it has" — the layout change that comment called
		// owner-gated is the one the owner later ruled on (devtest item 5, Option B). panel-top is
		// now a second handle inside the left sidebar, kept because removing it would take drag away
		// from that column for no gain.
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

	it('the layout drag strip survives as a fallback, but is no longer any route\'s only cover', () => {
		// Was: "still the only cover for Browse's right panel". Devtest item 5 ended that — Browse's
		// right panel now sits under the page's own full-width .topbar. The 8px strip stays because
		// it is a real (if thin) handle at the very top edge; it is just no longer load-bearing.
		const s = layoutSrc();
		expect(s).toContain('win-drag-top');
	});

	it('route topbars get right-padding so actions never slide under the controls', () => {
		// The :global(.topbar) padding rule is the one-site fix for the Contacts/Browse drift pair:
		// instead of editing each route's topbar, a single rule in the layout reserves the space.
		const s = layoutSrc();
		expect(s).toMatch(/:global\(\.topbar\)\s*\{[^}]*padding-right/);
	});

	it('Chat pane-header no longer reserves right-padding — the topbar above it does', () => {
		// INVERTED by devtest item 5. The 120px was there because the window controls overlaid
		// pane-header; they now overlay Chat's own .topbar, which sits above .chat-frame and picks
		// the reservation up from the layout's one :global(.topbar) rule. Leaving it would push
		// "View profile" 120px inward for no reason. If a future change moves the controls back over
		// the pane-header, this assertion is the one that should be flipped back — not silently
		// deleted.
		const src = readFileSync(new URL('./chat/+page.svelte', import.meta.url), 'utf8');
		expect(src).not.toMatch(/\.pane-header\s*\{[^}]*padding-right:\s*120px/);
	});
});
