//! Pure view-model for Private Collections (M10) — visibility defaults, the honest "not DRM" copy,
//! and the audience computation the collection manager + contacts UI share. No Svelte, no DOM, no
//! Tauri → unit-testable in the node env.

import type { Collection, Visibility } from './types.js';

/** Default visibility for a new / untouched collection — **Public**, never silently Private. */
export const DEFAULT_VISIBILITY: Visibility = 'Public';

/** The honest "this is not DRM" note, shown wherever Private collections are configured or shown.
 *  Trusted access is social trust: a trusted peer can copy/export, and revoking trust only affects
 *  **future** republishes — it cannot recall an already-fetched copy (spec §Private Collections /
 *  "What this is not"). The copy must keep both halves so the UI never over-promises. */
export const NOT_DRM_NOTE =
	'This is not DRM. A trusted contact can copy, screenshot, or export what you share. ' +
	'Removing trust only stops future republishes — it cannot un-send a copy they already fetched.';

/** A collection's effective visibility (absent ⇒ Public — a pre-M10 collection). */
export function visibilityOf(c: Pick<Collection, 'visibility'>): Visibility {
	return c.visibility ?? DEFAULT_VISIBILITY;
}

/** M21 W5: the Private audience is an explicit list of npubs (decoupled from contact groups by
 *  owner ruling 2026-08-04). The list is the de-duplicated audience. */
export function audienceRecipients(audience: string[]): string[] {
	const set = new Set<string>();
	for (const npub of audience) set.add(npub);
	return [...set];
}

/** Whether a contact (by npub) is in the Private audience — i.e. receives Private collections. */
export function receivesPrivate(npub: string, audience: string[]): boolean {
	return audience.includes(npub);
}
