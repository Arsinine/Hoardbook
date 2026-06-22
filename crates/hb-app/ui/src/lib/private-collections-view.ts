//! Pure view-model for Private Collections (M10) — visibility defaults, the honest "not DRM" copy,
//! and the trusted-recipient computation the collection manager + contacts UI share. No Svelte, no
//! DOM, no Tauri → unit-testable in the node env.

import type { Collection, Group, Visibility } from './types.js';

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

/** Whether a group is trusted (absent ⇒ false — trust is never granted by default). */
export function isTrusted(g: Pick<Group, 'trusted'>): boolean {
	return g.trusted === true;
}

/** The npubs that will receive a Private collection: the de-duplicated union of every trusted
 *  group's members. Empty ⇒ publishing a Private collection has no audience (the UI warns). */
export function trustedRecipients(groups: Group[]): string[] {
	const set = new Set<string>();
	for (const g of groups) if (isTrusted(g)) for (const p of g.pubkeys) set.add(p);
	return [...set];
}

/** Whether a contact (by npub) is in any trusted group — i.e. receives Private collections. */
export function contactIsTrusted(npub: string, groups: Group[]): boolean {
	return groups.some((g) => isTrusted(g) && g.pubkeys.includes(npub));
}
