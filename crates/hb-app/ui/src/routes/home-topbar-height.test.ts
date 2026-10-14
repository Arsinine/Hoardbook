// v0.24.0 devtest item 5 — "My Profile title bar container seems to be smaller than every other."
// Measured (vite + Playwright, 1280x800): Home's .topbar was 59.6px, every other page 77px. Home's
// subtitle is EMPTY once the profile is published and saved (the "Published…" line was removed
// 2026-10-13, pinned by q95-home-published-race), so the bar collapsed to the title alone. The fix
// reserves the subtitle line's height. After: 77px on Home, Settings; 77.8 on Contacts.
//
// Source scan, not a mount (P-4 note): jsdom computes no layout and this vitest setup does not
// inject the component's <style>, so neither height nor min-height is observable on a mounted
// page. Comments are stripped first (P-12) so this explanation cannot satisfy the scan.
// P-10 MUTATION (orchestrator): delete `min-height: 1lh;` from Home's .topbar-sub → REDS.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

describe('v0.24.0 item 5 — My Profile keeps the same title-bar height as every page', () => {
	it("Home's subtitle line reserves its height even when empty", () => {
		const src = readFileSync(resolve(process.cwd(), 'src/routes/+page.svelte'), 'utf8');
		const css = src.slice(src.indexOf('<style>')).replace(/\/\*[\s\S]*?\*\//g, '');
		const m = css.match(/\n\s*\.topbar-sub\s*\{([^}]*)\}/);
		expect(m, '.topbar-sub rule exists').not.toBeNull();
		expect((m as RegExpMatchArray)[1]).toMatch(/(^|;|\s)min-height:\s*1lh\s*;/);
	});
});
