// @vitest-environment jsdom
// QURATOR-352 — pins apply.ts: each root-attribute rule, the hoard attribute-REMOVAL, the
// localStorage mirror round-trip, the invalid-mirror fallback, and — because static/theme-boot.js
// is a deliberate hand-copy of apply.ts's semantics — the boot script's behavioural parity with
// apply.ts plus a list-level drift guard against themes.ts.
//
// Mutation proofs (run, then revert): (1) making applyAppearance set data-theme="hoard" instead
// of removing it reds "hoard removes data-theme"; (2) the float rule's "&& glass" term
// was the v0.24.0 rule; since the v0.24.0 devtest (glass changes materials only) RE-ADDING
// "&& glass" reds "data-glass-float iff the theme floats — the glass switch never changes it"; (3) in static/theme-boot.js, changing
// `var DEFAULT_THEME = 'vault';` back to 'hoard' reds "a first launch (no mirror) paints Vault".
//
// Owner ruling 2026-10-12: Vault is the DEFAULT theme. Hoard is the BASE theme (the bare :root, no
// attribute) and stays selectable. The tests below keep the two ideas apart.
import { readFileSync, readdirSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { describe, expect, it, beforeEach } from 'vitest';
import { THEMES, BASE_THEME, DEFAULT_THEME } from './themes.js';
import { applyAppearance, readMirroredAppearance, APPEARANCE_KEY, DEFAULT_APPEARANCE } from './apply.js';

const root = () => document.documentElement;
const has = (n: string) => root().hasAttribute(n);

function clearAll() {
	const r = document.documentElement;
	for (const n of ['data-theme', 'data-glass-float', 'data-glass-off', 'data-see-through']) r.removeAttribute(n);
	localStorage.clear();
}

function runBootScript() {
	const src = readFileSync(resolve(process.cwd(), 'static/theme-boot.js'), 'utf8');
	new Function(src)();
}

beforeEach(clearAll);

describe('applyAppearance — root attribute rules', () => {
	it('hoard removes data-theme even with a stale attribute present', () => {
		root().setAttribute('data-theme', 'vault');
		applyAppearance({ theme: 'hoard', glass: true });
		expect(has('data-theme')).toBe(false);
	});

	it('every theme other than the base palette sets data-theme (including the default, Vault)', () => {
		applyAppearance({ theme: 'vault', glass: true });
		expect(root().getAttribute('data-theme')).toBe('vault');
	});

	it('data-glass-float iff the theme floats — the glass switch never changes it', () => {
		// Owner, v0.24.0 devtest: "Enabling/Disabling Glass effects does not only change materials but
		// the overall layout from grids to bubbles." Glass off keeps the floating layout; only
		// data-glass-off (materials) changes.
		applyAppearance({ theme: 'vault', glass: true });
		expect(has('data-glass-float')).toBe(true);
		applyAppearance({ theme: 'vault', glass: false });
		expect(has('data-glass-float'), 'glass off keeps the floating layout').toBe(true);
		// Every theme floats, hoard included (owner, 2026-10-13), and keeps floating with glass off.
		applyAppearance({ theme: 'hoard', glass: true });
		expect(has('data-glass-float')).toBe(true);
		applyAppearance({ theme: 'hoard', glass: false });
		expect(has('data-glass-float'), 'hoard with glass off keeps the floating layout').toBe(true);
		applyAppearance({ theme: 'pale-pulse', glass: true });
		expect(has('data-glass-float')).toBe(true);
	});

	it('data-glass-off iff glass is off', () => {
		applyAppearance({ theme: 'vault', glass: false });
		expect(has('data-glass-off')).toBe(true);
		applyAppearance({ theme: 'vault', glass: true });
		expect(has('data-glass-off')).toBe(false);
	});

	it('the see-through window is gone: data-see-through is never set, and a stale one is scrubbed', () => {
		// Removed 2026-10-14 (owner: "We couldnt get the see-through window to work"). A root left
		// carrying data-see-through by an older build must lose it on the next applyAppearance.
		// P-10 MUTATION: delete `root.removeAttribute('data-see-through');` from apply.ts → REDS.
		root().setAttribute('data-see-through', '');
		applyAppearance({ theme: 'vault', glass: true });
		expect(has('data-see-through')).toBe(false);
		applyAppearance({ theme: 'hoard', glass: true });
		expect(has('data-see-through')).toBe(false);
	});
});

describe('applyAppearance — input validation (GLM review 2026-10-12)', () => {
	it('an unknown theme id falls back to the default instead of reaching the DOM', () => {
		// MUTATION (P-10): in applyAppearance, replace the isThemeId fallback with `theme: input.theme`
		// — this reds (data-theme becomes "midnight", which matches no theme file).
		applyAppearance({ theme: 'midnight' as never, glass: true });
		expect(root().getAttribute('data-theme')).toBe(DEFAULT_THEME);
		expect(JSON.parse(localStorage.getItem(APPEARANCE_KEY) ?? '').theme).toBe(DEFAULT_THEME);
	});
});

describe('hb-appearance mirror', () => {
	it('applyAppearance mirrors {theme, glass} to localStorage', () => {
		applyAppearance({ theme: 'vault', glass: false });
		const mirrored = JSON.parse(localStorage.getItem(APPEARANCE_KEY) ?? '');
		expect(mirrored).toEqual({ theme: 'vault', glass: false });
	});

	it('readMirroredAppearance round-trips a valid mirror', () => {
		// A mirror written by a v0.23.x build still carries seeThrough; the reader ignores the stale key.
		localStorage.setItem(APPEARANCE_KEY, JSON.stringify({ theme: 'oxblood-steel', glass: false, seeThrough: true }));
		expect(readMirroredAppearance()).toEqual({ theme: 'oxblood-steel', glass: false });
	});

	it('missing mirror falls back to the default triple', () => {
		expect(readMirroredAppearance()).toEqual(DEFAULT_APPEARANCE);
		expect(DEFAULT_APPEARANCE).toEqual({ theme: 'vault', glass: true });
		expect(DEFAULT_THEME).toBe('vault');
		expect(BASE_THEME).toBe('hoard');
	});

	it('garbage mirror falls back to the default triple', () => {
		localStorage.setItem(APPEARANCE_KEY, 'not json');
		expect(readMirroredAppearance()).toEqual({ theme: 'vault', glass: true });
	});

	it('mirror with an invalid theme falls back to the default triple', () => {
		localStorage.setItem(APPEARANCE_KEY, JSON.stringify({ theme: 'nope', glass: true }));
		expect(readMirroredAppearance()).toEqual({ theme: 'vault', glass: true });
	});
});

describe('static/theme-boot.js — parity with apply.ts', () => {
	it('paints the same root attributes as applyAppearance for the same mirror', () => {
		const combos: Array<{ theme: string; glass: boolean }> = [
			{ theme: 'vault', glass: true },
			{ theme: 'vault', glass: false },
			{ theme: 'hoard', glass: true },
			{ theme: 'hoard', glass: false },
			{ theme: 'pale-pulse', glass: true },
		];
		for (const c of combos) {
			clearAll();
			localStorage.setItem(APPEARANCE_KEY, JSON.stringify(c));
			runBootScript();
			const bootAttrs = ['data-theme', 'data-glass-float', 'data-glass-off', 'data-see-through'].map((n) => root().getAttribute(n));
			const bootTheme = root().getAttribute('data-theme');
			clearAll();
			applyAppearance({ theme: c.theme as never, glass: c.glass });
			const applyAttrs = ['data-theme', 'data-glass-float', 'data-glass-off', 'data-see-through'].map((n) => root().getAttribute(n));
			const applyTheme = root().getAttribute('data-theme');
			expect(bootTheme).toBe(applyTheme);
			expect(bootAttrs).toEqual(applyAttrs);
		}
	});

	it('a first launch (no mirror) paints Vault, the same as applyAppearance(DEFAULT_APPEARANCE)', () => {
		runBootScript();
		const boot = ['data-theme', 'data-glass-float', 'data-glass-off', 'data-see-through'].map((n) => root().getAttribute(n));
		expect(root().getAttribute('data-theme')).toBe('vault');
		clearAll();
		applyAppearance(DEFAULT_APPEARANCE);
		const viaApply = ['data-theme', 'data-glass-float', 'data-glass-off', 'data-see-through'].map((n) => root().getAttribute(n));
		expect(boot).toEqual(viaApply);
	});

	it('a corrupt mirror (JSON that fails to parse) still paints the default, never the base palette', () => {
		// MUTATION (P-10): empty the boot script's catch block — this reds (no data-theme at all).
		localStorage.setItem(APPEARANCE_KEY, '{not json');
		runBootScript();
		expect(root().getAttribute('data-theme')).toBe(DEFAULT_THEME);
		expect(has('data-glass-float')).toBe(true);
	});

	it('the boot script names the same base and default themes as themes.ts', () => {
		const src = readFileSync(resolve(process.cwd(), 'static/theme-boot.js'), 'utf8');
		expect(src).toContain(`var BASE_THEME = '${BASE_THEME}';`);
		expect(src).toContain(`var DEFAULT_THEME = '${DEFAULT_THEME}';`);
	});

	it('THEMES and float list in the boot script match themes.ts', () => {
		const src = readFileSync(resolve(process.cwd(), 'static/theme-boot.js'), 'utf8');
		const themes = (src.match(/var THEMES = \[([^\]]*)\]/) ?? [])[1] ?? '';
		const floating = (src.match(/var FLOATING = \[([^\]]*)\]/) ?? [])[1] ?? '';
		const parse = (s: string) => s.split(',').map((x) => x.trim().replace(/'/g, '')).filter(Boolean);
		expect(parse(themes)).toEqual(THEMES.map((t) => t.id));
		expect(parse(floating)).toEqual(THEMES.filter((t) => t.float).map((t) => t.id));
		expect(src).toContain("'hb-appearance'");
	});
});

describe('the see-through window (Mica) stays removed', () => {
	// Removed 2026-10-14 (owner: "We couldnt get the see-through window to work. Let's just remove it
	// altogether."). WHY A SOURCE WALK (P-4 note): the property is "no file anywhere SETS the
	// attribute or calls the OS material", which no single mounted page can observe. Comments are
	// stripped first (P-12), and `removeAttribute('data-see-through')` is legal — apply.ts and
	// theme-boot.js keep it as a scrub for roots an older build left marked.
	// P-10 MUTATIONS: (a) add `root.setAttribute('data-see-through', '');` to applyAppearance → REDS;
	// (b) re-add `export const setWindowMaterial = …` to src/lib/api.ts → REDS.
	const strip = (t: string) => t.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:])\/\/.*$/gm, '$1').replace(/<!--[\s\S]*?-->/g, '');
	function walk(dir: string, out: string[] = []): string[] {
		for (const e of readdirSync(dir, { withFileTypes: true })) {
			const p = join(dir, e.name);
			if (e.isDirectory()) walk(p, out);
			else if (/\.(ts|js|svelte|css|html)$/.test(e.name) && !/\.test\.ts$/.test(e.name)) out.push(p);
		}
		return out;
	}
	const files = [...walk(resolve(process.cwd(), 'src')), ...walk(resolve(process.cwd(), 'static'))];

	it('no source file sets data-see-through or calls the window material', () => {
		expect(files.length, 'the walk found the source tree').toBeGreaterThan(50);
		const hits: string[] = [];
		for (const f of files) {
			const code = strip(readFileSync(f, 'utf8'));
			if (/(setAttribute|setOrRemove)\(\s*['"]data-see-through/.test(code)) hits.push(`${f}: sets data-see-through`);
			if (/\[data-see-through\]/.test(code)) hits.push(`${f}: styles [data-see-through]`);
			if (/set_window_material|window_material_supported|setWindowMaterial|windowMaterialSupported/.test(code)) hits.push(`${f}: window-material call`);
		}
		expect(hits).toEqual([]);
	});
});
