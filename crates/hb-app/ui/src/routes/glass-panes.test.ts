// QURATOR-352 lane F — route pages become glass panes.
// Source-scan (not mount) is order-sanctioned HERE: the assertion is about MARKUP CLASSES on the
// topbar and the top-level column containers, not behaviour, and mounting six pages is heavy —
// a class-presence check gains nothing from a mount. Comments are stripped BEFORE scanning so a
// commented-out example (`class="topbar hb-pane"` in prose) can never satisfy the scan
// (P-12/P-13: strip comments before asserting; assert the exact markup shape, not a substring).
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

type Page = { route: string; file: string; columns: string[] };

// The columns are each page's TOP-LEVEL column containers (read from the markup, not guessed):
// home .body > .profile-pane + .collections-pane; browse .browse-shell > .left-panel + .right-panel;
// contacts .contacts-body > .contacts-col + .detail-pane | .discover-pane (QURATOR-360); chat .chat-frame > .convo-sidebar +
// .convo-pane; topics .master-detail > .list-pane + .detail-pane; settings .main > .body (single).
const PAGES: Page[] = [
	{ route: 'home', file: './+page.svelte', columns: ['profile-pane', 'collections-pane'] },
	{ route: 'browse', file: './browse/+page.svelte', columns: ['left-panel', 'right-panel'] },
	{ route: 'contacts', file: './contacts/+page.svelte', columns: ['contacts-col', 'detail-pane', 'discover-pane'] },
	{ route: 'chat', file: './chat/+page.svelte', columns: ['convo-sidebar', 'convo-pane'] },
	{ route: 'topics', file: './topics/+page.svelte', columns: ['list-pane', 'detail-pane'] },
	{ route: 'settings', file: './settings/+page.svelte', columns: ['body'] },
];

function stripComments(src: string): string {
	return src
		.replace(/<!--[\s\S]*?-->/g, ' ')
		.replace(/\/\*[\s\S]*?\*\//g, ' ')
		// `//` line comments, but never a `://` inside a URL string.
		.replace(/(^|[^:"'`\\])\/\/[^\n]*/g, '$1');
}

describe('QURATOR-352 lane F — route pages become glass panes', () => {
	for (const { route, file, columns } of PAGES) {
		it(`${route}: the topbar and every top-level column carry hb-pane`, () => {
			const src = stripComments(readFileSync(new URL(file, import.meta.url), 'utf8'));
			// The OPENING TAG must carry hb-pane — slice the tag, don't scan the whole file
			// (same lesson as q81's tag slice: a match anywhere else must not satisfy this).
			const tag = src.match(/<div class="topbar[^"]*"[^>]*>/);
			expect(tag, `${route} has no .topbar opening tag`).not.toBeNull();
			expect(tag![0], `${route} topbar must carry hb-pane`).toContain('hb-pane');
			// Keep the contracts the existing guards pin: `topbar` stays the FIRST class and the
			// drag region stays on the element.
			expect(tag![0].startsWith('<div class="topbar')).toBe(true);
			expect(tag![0]).toContain('data-tauri-drag-region');
			for (const col of columns) {
				expect(
					src,
					`${route}: .${col} must carry hb-pane in its opening tag`,
				).toMatch(new RegExp(`class="${col} hb-pane"`));
			}
		});
	}
});
