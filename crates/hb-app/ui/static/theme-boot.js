// QURATOR-352 — theme boot: paint the root attributes from the localStorage mirror BEFORE Svelte
// loads, so a relaunch with a non-default theme never flashes hoard from the first frame.
//
// Plain script, no imports/modules: it must run before any bundler module, and it runs as a
// STATIC file because Tauri's CSP is `script-src 'self'` — an INLINE script in app.html would be
// blocked by it (no 'unsafe-inline' for scripts). `<script src="/theme-boot.js">` is same-origin.
//
// This is a deliberate hand-copy of apply.ts's semantics (applyAppearance +
// readMirroredAppearance collapsed into one pass) — apply.test.ts pins apply.ts, and the
// boot-script drift guard in apply.test.ts pins the THEME + FLOAT lists below against
// themes.ts, so a new theme that forgets this file fails a test. Keep the two in step.
(function () {
	'use strict';
	// BASE = the original palette (bare :root, no attribute). DEFAULT = what a first launch gets
	// (owner ruling 2026-10-12: Vault). Keep both in step with themes.ts (pinned by apply.test.ts).
	var BASE_THEME = 'hoard';
	var DEFAULT_THEME = 'vault';
	// Keep in step with THEMES in src/lib/themes/themes.ts (pinned by apply.test.ts).
	var THEMES = ['hoard', 'oxblood-steel', 'lime-ledger', 'clinic-teal', 'violet-drive', 'vault', 'pale-pulse', 'sky-slate'];
	// Which of those float (panes over a backdrop with a gap) — mirrors `float: true` rows.
	var FLOATING = ['hoard', 'oxblood-steel', 'lime-ledger', 'clinic-teal', 'violet-drive', 'vault', 'pale-pulse', 'sky-slate'];
	var KEY = 'hb-appearance';
	var root = document.documentElement;
	function setOrRemove(name, present) {
		if (present) root.setAttribute(name, '');
		else root.removeAttribute(name);
	}
	function paint(theme, glass, seeThrough) {
		// ⚠ data-theme is a VALUED attribute (the CSS keys on :root[data-theme="<id>"]), so it is
		// set with the theme id, never presence-style; the three flags below are presence-style.
		if (theme !== BASE_THEME) root.setAttribute('data-theme', theme);
		else root.removeAttribute('data-theme');
		setOrRemove('data-glass-float', FLOATING.indexOf(theme) !== -1 && glass);
		setOrRemove('data-glass-off', !glass);
		setOrRemove('data-see-through', seeThrough && glass);
	}
	try {
		var raw = localStorage.getItem(KEY);
		// No mirror (first launch) → paint the DEFAULT theme, so a fresh install opens on Vault
		// rather than flashing the base palette until Svelte loads.
		var a = raw ? JSON.parse(raw) : {};
		if (typeof a !== 'object' || a === null) a = {};
		var theme = typeof a.theme === 'string' && THEMES.indexOf(a.theme) !== -1 ? a.theme : DEFAULT_THEME;
		var glass = a.glass === false ? false : true;
		var seeThrough = a.seeThrough === true;
		// Same semantics as applyAppearance — per-field fallback slices of the default triple.
		paint(theme, glass, seeThrough);
	} catch (e) {
		// Never throw at boot. A broken mirror (JSON.parse threw) paints the default, the same
		// fallback readMirroredAppearance uses, so the first frame is never the base palette.
		try { paint(DEFAULT_THEME, true, false); } catch (e2) { /* nothing more to try */ }
	}
})();
