// QURATOR-352 — apply the appearance (theme / glass) as root attributes on
// <html>, and mirror it to localStorage so the theme-boot script can paint before Svelte loads.
// Settings is the source of truth; the mirror is a production paint-before-JS snapshot, never read
// back into Settings.
//
// The bare `:root` in app.css IS `hoard` (BASE_THEME), so applying it REMOVES `data-theme`
// rather than setting it to 'hoard' (no theme CSS keys on `:root[data-theme="hoard"]` — a stale
// attribute would match nothing and could stick across a relaunch).

import { BASE_THEME, DEFAULT_THEME, isThemeId, themeInfo, type ThemeId } from './themes.js';

export interface Appearance {
	theme: ThemeId;
	glass: boolean;
}

export const APPEARANCE_KEY = 'hb-appearance';

/** The appearance an invalid mirror falls back to (each field's fallback is this triple's slice). */
export const DEFAULT_APPEARANCE: Appearance = { theme: DEFAULT_THEME, glass: true };

export function applyAppearance(input: Appearance): void {
	const root = document.documentElement;
	// Settings round-trips any string, so an unknown id (a hand-edited or downgraded settings.json)
	// must not reach the DOM: `data-theme="midnight"` matches no theme file and would silently
	// render the base palette while the picker shows the default selected. Same fallback as the
	// boot script and readMirroredAppearance.
	const a: Appearance = { ...input, theme: isThemeId(input.theme) ? input.theme : DEFAULT_THEME };

	// The base (original) theme is the ABSENCE of the attribute. The DEFAULT theme (Vault) is a
	// theme file like any other and gets the attribute.
	if (a.theme === BASE_THEME) root.removeAttribute('data-theme');
	else root.setAttribute('data-theme', a.theme);

	// Panes float (gap + rim, hb-pane) whenever the theme floats — glass on OR off. The glass switch
	// changes materials only, never the layout (owner, v0.24.0 devtest).
	if (themeInfo(a.theme).float) root.setAttribute('data-glass-float', '');
	else root.removeAttribute('data-glass-float');

	// Glass off: .hb-pane/.hb-raised fall back to opaque surfaces (app.css handles the visuals).
	if (!a.glass) root.setAttribute('data-glass-off', '');
	else root.removeAttribute('data-glass-off');

		// The see-through window was removed 2026-10-14 (owner: we could not get it to work).
		// data-see-through is gone; scrub any stale attribute an older build may have left -
		// removeAttribute is a no-op when the attribute is absent.
		root.removeAttribute('data-see-through');

	// Best-effort mirror for the boot script. try/catch: disabled/private-mode storage must never
	// break applying the appearance itself.
	try {
		localStorage.setItem(APPEARANCE_KEY, JSON.stringify({ theme: a.theme, glass: a.glass }));
	} catch {
		// Storage write failed — the mirror is best-effort by design; the attributes are already set.
	}
}

export function readMirroredAppearance(): Appearance {
	try {
		const raw = localStorage.getItem(APPEARANCE_KEY);
		if (raw === null) return { ...DEFAULT_APPEARANCE };
		const v: unknown = raw === '' ? null : JSON.parse(raw);
		if (typeof v !== 'object' || v === null) return { ...DEFAULT_APPEARANCE };
		const o = v as { theme?: object; glass?: unknown };
		// Per-field validation: each invalid field takes its slice of DEFAULT_APPEARANCE (so a
		// mirror invalid only in theme still falls back to the exact triple the order names).
		// The theme field is the one pinned to isThemeId; booleans must be actual booleans so a
		// stringified "false" cannot silently disable glass.
		return {
			theme: isThemeId(o.theme) ? o.theme : DEFAULT_THEME,
			glass: typeof o.glass === 'boolean' ? o.glass : true,
		};
	} catch {
		return { ...DEFAULT_APPEARANCE };
	}
}
