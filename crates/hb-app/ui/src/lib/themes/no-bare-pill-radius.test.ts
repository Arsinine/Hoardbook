// QURATOR-382 lane D — ALLOWLIST guard: no NEW bare pill radius outside the list below.
//
// Five themes (vault, clinic-teal, pale-pulse, sky-slate, violet-drive — lib/themes/*.css) set
// `--r-control: 999px; --r-chip: 999px;`, so any element that uses those tokens BARE silently
// becomes a stadium. The fix for a non-control is the app.css `.hb-textarea` / QURATOR-381
// (c84d4678) precedent:
//
//     border-radius: min(var(--r-control, <site fallback>), var(--r-card, 10px));
//
// This guard inverts the per-site fix list (CLAUDE.md §9: a hand-written list of sites cannot fix
// a hand-written list of sites). It scans every svelte <style> block under src/routes and
// src/lib/components plus src/app.css (block comments stripped first, P-12), finds every
// `border-radius:` declaration whose value contains `var(--r-control` / `var(--r-chip` and is NOT
// wrapped in `min(`, attributes it to its enclosing selector, and demands that file+selector be in
// ALLOWED.
//
// TO ADD A BARE SITE: it must be a genuine SINGLE-LINE control (button, chip, toggle, input,
// menu item, badge) — otherwise cap it with min(var(--r-control, …), var(--r-card, 10px)) and add
// nothing here. Conversely, capping or deleting a listed site REQUIRES removing its ALLOWED entry:
// stale entries red (assertion (b)), which is what keeps this list honest.
//
// .hb-input itself is allowed bare — a single-line input is a control by design (owner: pill
// controls). The two lane-D ADDS — CollectionTagsEditor `.tag-wrap` and AddCollectionModal
// `.lang-wrap` — are exactly why a WRAPPER THAT WRAPS must override it: both had no border-radius
// of their own, inherited .hb-input's bare token from app.css, and went stadium on 999px themes
// once their flex-wrapped content grew past one line. Both now declare the min() cap themselves.
//
// ⚠ P-13 — declared-CSS check: jsdom computes no layout, so this pins source text, not rendering.
//
// MUTATION (P-10), 2026-10-18, both seen red:
// (i) AddCollectionModal .tree-box temporarily reverted to bare `var(--r-control, 7px)` → RED
//     naming it under unexpectedBare (laneD-guard-red.log, exit=1);
// (ii) the ALLOWED '.win-btn' entry deleted → RED too, but via assertion (a) (the real bare site
//     becomes unallowed). To pin assertion (b) itself, the entry was then RENAMED to a
//     never-occurring '.win-btn-typo' → RED with staleAllowEntries naming it
//     (laneD-guard-red2-stale.log, exit=1). Restored to '.win-btn' → GREEN.
import { describe, expect, it } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

// .../ui/src — vitest root is the ui/ dir (see vite.config.ts test.include)
const SRC = join(process.cwd(), 'src');
const ROOTS = ['routes', 'lib/components'];

/**
 * Selectors permitted to keep a BARE `var(--r-control` / `var(--r-chip` border-radius, keyed by
 * path relative to src/ (posix separators). Selector text is matched EXACTLY as the scanner
 * attributes it (the line before the rule's `{`, trimmed) — a multi-selector rule is one entry
 * (`.cb-yes, .cb-no`). Anything not listed must use min(..., var(--r-card, 10px)).
 */
const ALLOWED: Record<string, string[]> = {
	// app.css — shared primitives: buttons/inputs/chips are genuine single-line controls
	'app.css': ['.btn-danger', '.hb-input', '.hb-tag'],
	// AZRail — sidebar rail buttons
	'lib/components/AZRail.svelte': ['.az-rail-btn'],
	// AddCollectionModal — category toggle + removable tag chip (the .tree-box and .lang-wrap
	// wrappers are CAPPED, not here)
	'lib/components/AddCollectionModal.svelte': ['.ct-toggle', '.chip'],
	// CollectionRow — status chip, row menu button, menu item
	'lib/components/CollectionRow.svelte': ['.chip-status', '.row-menu-btn', '.menu-item'],
	// CollectionTagsEditor — the tag chip itself (its WRAPPER .tag-wrap is capped — see header)
	'lib/components/CollectionTagsEditor.svelte': ['.chip'],
	// ConfirmButton — trigger and its inline yes/no confirm buttons
	'lib/components/ConfirmButton.svelte': ['.cb-trigger', '.cb-yes, .cb-no'],
	// DiscoverBook — pills by design (their own fallback is already 999px)
	'lib/components/DiscoverBook.svelte': ['.title-chip', '.chip'],
	// TopicJoinConsent — action buttons
	'lib/components/TopicJoinConsent.svelte': ['.actions button'],
	// WindowControls — window chrome buttons
	'lib/components/WindowControls.svelte': ['.win-btn'],
	// layout — toast action button
	'routes/+layout.svelte': ['.toast-action'],
	// home — icon button, language tag, willingness button, badge, add button
	'routes/+page.svelte': ['.social-icon-btn', '.lang-tag', '.willing-btn', '.ct-badge', '.btn-add'],
	// browse — scope chip, progress tracks (6px bars: pill ends are intended), filter button, view toggle, sort button
	'routes/browse/+page.svelte': ['.group-scope-btn', '.paywall-progress-track', '.fetch-track', '.bc-btn', '.view-toggle', '.sort-dir'],
	// chat — menu item + send button
	'routes/chat/+page.svelte': ['.menu-item', '.btn-send'],
	// contacts — view toggle, row menu, menu item, row action, collision badge, show-more,
	// tag input, copy button
	'routes/contacts/+page.svelte': ['.view-toggle', '.row-menu-btn', '.menu-item', '.row-act', '.collision-badge', '.show-more', '.tag-input', '.copy-btn'],
};

function walk(dir: string, out: string[] = []): string[] {
	for (const e of readdirSync(dir, { withFileTypes: true })) {
		const p = join(dir, e.name);
		if (e.isDirectory()) walk(p, out);
		else if (e.name.endsWith('.svelte')) out.push(p);
	}
	return out;
}

/** Every bare r-control/r-chip border-radius in one file's CSS. `whole` = app.css (no <style>). */
function barePillSites(text: string, whole: boolean): { sel: string; decl: string }[] {
	const blocks = whole
		? [text]
		: [...text.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)].map((m) => m[1]);
	const hits: { sel: string; decl: string }[] = [];
	for (let b of blocks) {
		b = b.replace(/\/\*[\s\S]*?\*\//g, ''); // P-12: strip comments BEFORE scanning
		let idx = 0;
		for (;;) {
			const di = b.indexOf('border-radius:', idx);
			if (di < 0) break;
			const semi = b.indexOf(';', di);
			const val = b.slice(di + 'border-radius:'.length, semi < 0 ? b.length : semi).trim();
			idx = semi < 0 ? b.length : semi + 1;
			if (!(val.includes('var(--r-control') || val.includes('var(--r-chip'))) continue;
			if (/\bmin\s*\(/.test(val)) continue;
			const open = b.lastIndexOf('{', di);
			hits.push({ sel: b.slice(0, open).trim().split('\n').pop()!.trim(), decl: `border-radius: ${val}` });
		}
	}
	return hits;
}

describe('no bare pill radius outside the allowlist (999px-theme stadium guard)', () => {
	const files = ROOTS.flatMap((r) => walk(join(SRC, r))).sort();
	const keyOf = (abs: string) => abs.slice(SRC.length + 1).split('\\').join('/');
	// app.css is a scan target in its own right, alongside the walked svelte files
	const targets: { key: string; text: string; whole: boolean }[] = [
		{ key: 'app.css', text: readFileSync(join(SRC, 'app.css'), 'utf8'), whole: true },
		...files.map((f) => ({ key: keyOf(f), text: readFileSync(f, 'utf8'), whole: false })),
	];

	it('examines a sane number of files', () => {
		// assertion (c): "0 hits" must never mean "scanned nothing" (CLAUDE.md §7 sweep rule)
		console.log(`examined ${targets.length} style sources (svelte routes + lib/components + app.css)`);
		expect(targets.length).toBeGreaterThan(20);
		for (const must of ['app.css', 'routes/+page.svelte', 'routes/contacts/+page.svelte', 'lib/components/AddCollectionModal.svelte', 'lib/components/CollectionRow.svelte']) {
			expect(targets.some((t) => t.key === must), `coverage: ${must} was NOT examined (0-files-examined guard)`).toBe(true);
		}
	});

	it('finds no bare r-control/r-chip radius outside ALLOWED; every ALLOWED entry is still found bare', () => {
		const unexpected: string[] = [];
		const found = new Set<string>();
		for (const t of targets) {
			for (const h of barePillSites(t.text, t.whole)) {
				found.add(`${t.key}|${h.sel}`);
				if (!(ALLOWED[t.key] ?? []).includes(h.sel)) {
					unexpected.push(`${t.key} — ${h.sel} — ${h.decl}\n    fix: wrap it in min(var(--r-control, <fallback>), var(--r-card, 10px)) OR, if it is a genuine single-line control, add "${h.sel}" to ALLOWED in ${import.meta.filename}`);
				}
			}
		}
		const stale: string[] = [];
		for (const [key, sels] of Object.entries(ALLOWED)) {
			for (const sel of sels) {
				if (!found.has(`${key}|${sel}`)) stale.push(`${key} — ${sel} (no longer found bare: capped, deleted, or selector text changed — update ALLOWED)`);
			}
		}
		// ONE combined assert so a double mutation shows both halves in a single red
		expect(
			{ unexpectedBare: unexpected, staleAllowEntries: stale },
			`bare-pill guard: ${unexpected.length} unexpected, ${stale.length} stale\n${[...unexpected, ...stale].join('\n')}`
		).toEqual({ unexpectedBare: [], staleAllowEntries: [] });
	});
});
