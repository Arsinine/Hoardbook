// QURATOR-352 — the theme contrast gate. Every theme must hold WCAG AA (4.5:1) for every text
// role over its glass, in BOTH modes:
//   normal      — text over (glass over each backdrop sample);
//   see-through — text over (veil over glass over (sample at 85 % over WHITE)) and over
//                 (… over BLACK). `hoard` has no veil (its --glass-veil is transparent) and is
//                 skipped here by design.
//   plus        — --accent-text over every --primary-fill stop.
// This parses the REAL CSS, not a copy: `hoard` is the bare `:root` block of src/app.css, every
// other theme is its own `<id>.css` beside this test. Gradients (--glass, --primary-fill) are
// checked at EVERY colour stop, not just the first. A missing/misformatted token throws — the
// parser must never silently skip a role, or a deleted token would read as a pass.
//
// The backdrop samples and the veil alphas are the draft's own validated numbers, mirrored here
// in the SAMPLES / VEIL tables below (draft: /home/zephyrus/hoardbook-drafts/352-r2/index.html,
// `const T[].samples` and `const VEIL`). The `SAMPLES mirror THEMES` test fails loudly when a new
// theme is added without a row, so the table can never go stale silently.
//
// The oklch→sRGB converter is COPIED from src/lib/contrast.test.ts (it is not exported there,
// and importing a helper out of another suite would couple two test files); its encode step is
// extended to gamma-encoded 0-255 because the compositing maths below runs in gamma space.
//
// P-10 mutation ledger (each proven red against this gate, then reverted byte-for-byte):
//   1. vault.css --fg-dim → rgb(255 255 255 / .30)          → vault rows red;
//   2. sky-slate.css --glass-veil → transparent              → sky-slate see-through rows red;
//   3. lime-ledger.css --accent line deleted                  → parser throws loudly (red).
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join, dirname } from 'node:path';
import { THEMES } from './themes';
import type { ThemeId } from './themes';

const THEMES_DIR = dirname(fileURLToPath(import.meta.url));
const APP_CSS = join(THEMES_DIR, '..', '..', 'app.css');
const MIN_RATIO = 4.5;

type RGBA = [number, number, number, number];

/* ── colour maths — `over`, `lum` and `ratio` are the draft's functions, ported verbatim ── */

/** Alpha-composite `top` over `bot`, in gamma-encoded 0-255 space (as browsers do for CSS colours). */
function over(top: RGBA, bot: RGBA): RGBA {
	const a = top[3];
	return [
		top[0] * a + bot[0] * (1 - a),
		top[1] * a + bot[1] * (1 - a),
		top[2] * a + bot[2] * (1 - a),
		1,
	];
}

/** WCAG 2.x relative luminance from gamma-encoded 0-255 sRGB channels. */
function lum(c: RGBA): number {
	const f = (v: number) => {
		v /= 255;
		return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
	};
	return 0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2]);
}

function ratio(a: RGBA, b: RGBA): number {
	const x = lum(a);
	const y = lum(b);
	return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

/** oklch → linear-light sRGB via standard OKLab math (Björn Ottosson), gamut-clamped.
 *  Copied verbatim from src/lib/contrast.test.ts. */
function oklchToSrgbLinear(L: number, C: number, hueDeg: number): [number, number, number] {
	const h = (hueDeg * Math.PI) / 180;
	const a = C * Math.cos(h);
	const b = C * Math.sin(h);
	const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
	const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
	const s_ = L - 0.0894841775 * a - 1.291485548 * b;
	const l = l_ ** 3;
	const m = m_ ** 3;
	const s = s_ ** 3;
	const clamp = (v: number) => Math.min(1, Math.max(0, v));
	return [
		clamp(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
		clamp(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
		clamp(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
	];
}

/** Linear-light sRGB channel → gamma-encoded 0-255 (the IEC sRGB encode curve). */
function srgbEncode(v: number): number {
	const e = v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
	return Math.round(255 * Math.min(1, Math.max(0, e)));
}

/* ── real-CSS parsing ── */

const COLOUR_RE = /#[0-9a-fA-F]{6}\b|rgba?\([^)]*\)|oklch\([^)]*\)/g;

/** Parse one colour in any syntax these stylesheets actually use: #rrggbb, rgb(r g b / a)
 *  (legacy commas also accepted), oklch(L C H [/ a]), or the `transparent` keyword.
 *  Anything else throws — unknown syntax must be loud, never skipped. */
function parseColour(raw: string, where: string): RGBA {
	const c = raw.trim();
	if (/^transparent$/i.test(c)) return [0, 0, 0, 0];
	if (c[0] === '#') {
		const h = c.slice(1);
		return [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16), 1];
	}
	const rgb = /^rgba?\(\s*([^)]*)\)$/.exec(c);
	if (rgb) {
		const p = rgb[1]
			.trim()
			.split(/[\s,/]+/)
			.filter(Boolean)
			.map(Number);
		if ((p.length !== 3 && p.length !== 4) || p.some((v) => Number.isNaN(v))) {
			throw new Error(`${where}: cannot parse rgb() colour "${raw}"`);
		}
		return [p[0], p[1], p[2], p.length === 4 ? p[3] : 1];
	}
	const ok = /^oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*([\d.]+))?\s*\)$/.exec(c);
	if (ok) {
		const [r, g, b] = oklchToSrgbLinear(parseFloat(ok[1]), parseFloat(ok[2]), parseFloat(ok[3]));
		return [srgbEncode(r), srgbEncode(g), srgbEncode(b), ok[4] !== undefined ? parseFloat(ok[4]) : 1];
	}
	throw new Error(`${where}: unsupported colour syntax "${raw}" — parser fails loudly, never silently skips`);
}

/** Slice ONE rule block out of a stylesheet by brace matching. Throws if the selector is absent. */
function cssBlock(css: string, selectorRe: RegExp, where: string): string {
	const m = selectorRe.exec(css);
	if (!m) throw new Error(`${where}: block not found`);
	const open = m.index + m[0].length - 1; // position of '{'
	let depth = 0;
	for (let i = open; i < css.length; i++) {
		if (css[i] === '{') depth++;
		else if (css[i] === '}') {
			depth--;
			if (depth === 0) return css.slice(open + 1, i);
		}
	}
	throw new Error(`${where}: unterminated block`);
}

/** Every `--token: value;` declaration in a block's text. */
function declarations(blockText: string): Map<string, string> {
	const out = new Map<string, string>();
	for (const m of blockText.matchAll(/--([\w-]+)\s*:\s*([^;]+);/g)) out.set(m[1], m[2].trim());
	return out;
}

/** Resolve a bare `var(--x)` (optionally chained) against the SAME block's declarations.
 *  hoard needs this: `--glass: var(--bg)` and `--primary-fill: var(--accent)`. */
function resolveVar(value: string, toks: Map<string, string>, where: string, depth = 0): string {
	const m = /^var\(\s*--([\w-]+)\s*\)$/.exec(value.trim());
	if (!m) return value.trim();
	if (depth > 4) throw new Error(`${where}: var() chain too deep at --${m[1]}`);
	const next = toks.get(m[1]);
	if (next === undefined) throw new Error(`${where}: var(--${m[1]}) does not resolve in this block`);
	return resolveVar(next, toks, where, depth + 1);
}

/** A theme's token map, read from the real CSS. `hoard` = the bare `:root` of app.css;
 *  every other theme = its own `:root[data-theme="<id>"]` block. */
function themeTokens(id: ThemeId): Map<string, string> {
	if (id === 'hoard') {
		return declarations(cssBlock(readFileSync(APP_CSS, 'utf8'), /(?:^|\n)\s*:root\s*\{/, 'hoard (:root in app.css)'));
	}
	const path = join(THEMES_DIR, `${id}.css`);
	return declarations(cssBlock(readFileSync(path, 'utf8'), new RegExp(`(?:^|\\n)\\s*:root\\[data-theme="${id}"\\]\\s*\\{`), id));
}

/** The colour stops of a token value: every colour inside a linear-gradient, else the one colour. */
function colourStops(value: string, where: string): RGBA[] {
	if (/gradient\(/i.test(value)) {
		const found = value.match(COLOUR_RE);
		if (!found || found.length === 0) throw new Error(`${where}: gradient with no parsable colour stops "${value}"`);
		return found.map((c) => parseColour(c, where));
	}
	return [parseColour(value, where)];
}

/* ── backdrop samples + veils, mirrored from the draft (/home/zephyrus/hoardbook-drafts/352-r2/
   index.html `const T[].samples` and `const VEIL`). Each list is the theme's darkest/brightest
   backdrop regions. ADD A ROW FOR EVERY NEW THEME — the `SAMPLES mirror THEMES` test fails
   loudly on a missing or extra id. ── */
const SAMPLES: Record<ThemeId, string[]> = {
	hoard: ['#0e0f11'],
	'oxblood-steel': ['#0f0b11', '#4e0613', '#54424a'],
	'lime-ledger': ['#050c0a', '#2b3d12', '#3a4f1c'],
	'clinic-teal': ['#163244', '#1f465d', '#2a5976'],
	'violet-drive': ['#0f0424', '#5c2598', '#4d137f'],
	vault: ['#07070a', '#594a51', '#3a3036'],
	'pale-pulse': ['#b3c8e2', '#8aa3c9', '#9a93b0'],
	'sky-slate': ['#c5dce4', '#60899c', '#7fa3b6'],
};

/** The draft's VEIL: the least extra tint (black on dark themes, white on light) that keeps every
 *  role at AA with a white or black desktop showing through at 15 %. */
const VEIL: Record<ThemeId, number> = {
	hoard: 0,
	'oxblood-steel': 0.22,
	'lime-ledger': 0.24,
	'clinic-teal': 0.14,
	'violet-drive': 0.16,
	vault: 0.05,
	'pale-pulse': 0.18,
	'sky-slate': 0.12,
};

const DESKS: { label: string; c: RGBA }[] = [
	{ label: 'WHITE', c: [255, 255, 255, 1] },
	{ label: 'BLACK', c: [0, 0, 0, 1] },
];

interface Row {
	theme: string;
	role: string;
	mode: string;
	ratio: number;
	pane: string;
}

/** Run every contrast check for one theme and return a row per (role × mode × worst-pane). */
function auditTheme(id: ThemeId): Row[] {
	const where = `theme ${id}`;
	const toks = themeTokens(id);
	for (const name of ['fg', 'fg-muted', 'fg-dim', 'accent', 'accent-text', 'glass', 'glass-veil', 'bg']) {
		if (!toks.has(name)) {
			throw new Error(`${where}: --${name} is missing from its CSS block — failing loudly, not silently skipping`);
		}
	}
	const colour = (name: string, fallback?: string): string => {
		const raw = toks.get(name) ?? (fallback !== undefined ? toks.get(fallback) : undefined);
		if (raw === undefined) throw new Error(`${where}: --${name} missing (fallback --${fallback} also missing)`);
		return resolveVar(raw, toks, where);
	};
	const roles: [string, RGBA][] = [
		['--fg', parseColour(colour('fg'), where)],
		['--fg-muted', parseColour(colour('fg-muted'), where)],
		['--fg-dim', parseColour(colour('fg-dim'), where)],
		['--accent', parseColour(colour('accent'), where)],
	];
	const ink = parseColour(colour('accent-text'), where);
	const glassStops = colourStops(colour('glass'), `${where} --glass`);
	// --primary-fill falls back to --accent (hoard sets it to var(--accent); most themes inherit).
	const primaryStops = colourStops(colour('primary-fill', 'accent'), `${where} --primary-fill`);
	const samples = SAMPLES[id].map((s) => parseColour(s, `SAMPLES[${id}]`));
	// The veil is the theme's REAL --glass-veil token, not the VEIL table — the table exists only
	// as a drift pin (see the mirror test). The M2 mutation (sky-slate --glass-veil → transparent)
	// only bites when the CSS value is what the maths consumes.
	const veil = parseColour(resolveVar(toks.get('glass-veil')!, toks, where), `${where} --glass-veil`);

	const rows: Row[] = [];
	const check = (role: string, text: RGBA, pane: RGBA, paneDesc: string, modeLabel: string) => {
		rows.push({ theme: id, role, mode: modeLabel, ratio: ratio(over(text, pane), pane), pane: paneDesc });
	};

	// Normal mode: text over (glass over each backdrop sample), at EVERY glass stop.
	for (const [gi, g] of glassStops.entries()) {
		for (const [si, s] of samples.entries()) {
			const pane = over(g, s);
			const desc = `glass stop ${gi + 1}/${glassStops.length} x sample ${si + 1}/${samples.length}`;
			for (const [role, text] of roles) check(role, text, pane, desc, 'normal');
		}
	}

	// See-through mode (hoard skipped: its veil is transparent and panes stay opaque).
	if (id !== 'hoard') {
		for (const [gi, g] of glassStops.entries()) {
			for (const [si, s] of samples.entries()) {
				for (const desk of DESKS) {
					const ground = over([s[0], s[1], s[2], 0.85], desk.c);
					const pane = over(veil, over(g, ground));
					const desc = `glass stop ${gi + 1}/${glassStops.length} x sample ${si + 1}/${samples.length} over ${desk.label} desk`;
					for (const [role, text] of roles) check(role, text, pane, desc, `see-through(${desk.label})`);
				}
			}
		}
	}

	// Button ink on the primary fill, at every stop.
	for (const [pi, p] of primaryStops.entries()) {
		rows.push({
			theme: id,
			role: '--accent-text',
			mode: 'primary-fill',
			ratio: ratio(ink, p),
			pane: `--primary-fill stop ${pi + 1}/${primaryStops.length}`,
		});
	}
	return rows;
}

function renderTable(rows: Row[]): string {
	const lines = rows.map(
		(r) => `${r.theme} | ${r.role} | ${r.mode} | ${r.ratio.toFixed(2)}:1 | ${r.pane}`
	);
	return ['theme | role | mode | ratio | pane', ...lines].join('\n');
}

describe('themes-contrast (QURATOR-352)', () => {
	it('SAMPLES and VEIL mirror exactly the ids in THEMES (a new theme without rows fails here)', () => {
		const ids = THEMES.map((t) => t.id).sort();
		expect(Object.keys(SAMPLES).sort(), 'SAMPLES table ids must equal THEMES ids').toEqual(ids);
		expect(Object.keys(VEIL).sort(), 'VEIL table ids must equal THEMES ids').toEqual(ids);
		// Drift pin: each theme's real --glass-veil token must equal the draft-mirrored VEIL alpha
		// (hoard stays transparent — its panes never go see-through). If a theme re-tunes its veil,
		// this names the theme and both values so the mirror is updated deliberately, never rots.
		for (const t of THEMES) {
			const toks = themeTokens(t.id);
			const where = `theme ${t.id}`;
			const raw = resolveVar(toks.get('glass-veil')!, toks, where);
			if (t.id === 'hoard') {
				expect(raw, 'hoard --glass-veil must stay transparent').toMatch(/^transparent$/i);
			} else {
				const a = parseColour(raw, `${where} --glass-veil`)[3];
				expect(
					a,
					`theme ${t.id}: --glass-veil alpha ${a} must equal the draft-mirrored VEIL value ${VEIL[t.id]} — update both together`
				).toBeCloseTo(VEIL[t.id], 5);
			}
		}
	});

	for (const t of THEMES) {
		it(`${t.id}: every text role clears 4.5:1 (normal + see-through + primary-fill ink)`, () => {
			const rows = auditTheme(t.id);
			expect(rows.length, `${t.id}: no contrast checks ran — parser produced nothing`).toBeGreaterThan(0);
			const failing = rows.filter((r) => r.ratio < MIN_RATIO);
			const table = renderTable(rows);
			if (failing.length > 0) {
				// Print EVERY ratio on failure so the failing rows say exactly what to fix.
				console.error(`\n[themes-contrast] ${t.id} — every ratio:\n${table}\n`);
			}
			expect(
				failing.map((f) => `${f.theme} ${f.role} ${f.mode} ${f.ratio.toFixed(2)}:1 over ${f.pane}`),
				`contrast rows below 4.5:1:\n${table}`
			).toEqual([]);
		});
	}
});
