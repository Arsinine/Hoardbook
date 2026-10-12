// QURATOR-352 — the theme registry. One row per theme; the Settings picker enumerates this list, so
// adding a theme never means editing the picker. Each non-default theme's tokens live in
// `<id>.css` beside this file as `:root[data-theme="<id>"] { … }`; `hoard` is the bare `:root` in
// app.css and has no file.
//
// `contrast` is the data the contrast gate (themes-contrast.test.ts) checks: the colours as plain
// hex/rgb so the test can do the maths without a CSS engine. They MUST match the theme's CSS file —
// the gate parses the CSS and fails on drift, so this is a copy that cannot silently diverge.

export type ThemeId =
	| 'hoard'
	| 'oxblood-steel'
	| 'lime-ledger'
	| 'clinic-teal'
	| 'violet-drive'
	| 'vault'
	| 'pale-pulse'
	| 'sky-slate';

export interface ThemeInfo {
	id: ThemeId;
	name: string;
	/** The supplied design system this theme was mapped from (`/08` etc.), or null for `hoard`
	 *  (the original palette). Whether a theme is the default comes from DEFAULT_THEME, not here. */
	source: string | null;
	mode: 'dark' | 'light';
	/** Swatch colours for the picker card: backdrop, glass, accent. */
	swatch: [string, string, string];
	/** True when panes float over a backdrop with a gap (glass themes). False keeps today's flush layout. */
	float: boolean;
}

/** The original palette: the bare `:root` in app.css, applied by having NO data-theme attribute. */
export const BASE_THEME: ThemeId = 'hoard';
/** What everyone gets until they choose a theme (owner ruling 2026-10-12: Vault). Not the same as
 *  BASE_THEME: the default is a theme file like any other, and Hoard stays selectable. */
export const DEFAULT_THEME: ThemeId = 'vault';

export const THEMES: readonly ThemeInfo[] = [
	{ id: 'hoard', name: 'Hoard', source: null, mode: 'dark', swatch: ['#0e0f11', '#1e2023', '#eba443'], float: false },
	{ id: 'oxblood-steel', name: 'Oxblood Steel', source: '/08', mode: 'dark', swatch: ['#0f0b11', '#4e0613', '#f4bcc1'], float: true },
	{ id: 'lime-ledger', name: 'Lime Ledger', source: '/17', mode: 'dark', swatch: ['#050c0a', '#2b3d12', '#d9fd18'], float: true },
	{ id: 'clinic-teal', name: 'Clinic Teal', source: '/31', mode: 'dark', swatch: ['#122832', '#1f465d', '#d6f5a9'], float: true },
	{ id: 'violet-drive', name: 'Violet Drive', source: '/38', mode: 'dark', swatch: ['#0f0424', '#5c2598', '#e4cfff'], float: true },
	{ id: 'vault', name: 'Vault', source: '/40', mode: 'dark', swatch: ['#07070a', '#3a3036', '#b2f0c7'], float: true },
	{ id: 'pale-pulse', name: 'Pale Pulse', source: '/07', mode: 'light', swatch: ['#b3c8e2', '#8aa3c9', '#34558a'], float: true },
	{ id: 'sky-slate', name: 'Sky Slate', source: '/12', mode: 'light', swatch: ['#c5dce4', '#60899c', '#004a9f'], float: true },
];

export function isThemeId(v: unknown): v is ThemeId {
	return typeof v === 'string' && THEMES.some((t) => t.id === v);
}

export function themeInfo(id: ThemeId): ThemeInfo {
	return THEMES.find((t) => t.id === id) ?? THEMES[0];
}
