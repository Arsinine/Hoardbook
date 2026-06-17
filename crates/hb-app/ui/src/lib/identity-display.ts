// Impersonation-resistant identity display (M3, AB4b) — the browse UI's trust layer, display &
// labelling ONLY (no contact CRUD; that is M5). Pure view-model logic so it is unit-tested with
// vitest, keeping the Svelte components thin.
//
// Two guarantees:
//  - A petname binds to the `npub`, NOT the display name. A teaser carrying a saved contact's name
//    but a different key does not inherit the petname — it is flagged as a likely impersonator.
//  - The word+color fingerprint is computed ONCE, in Rust (`hb_core::fingerprint`), and reaches the
//    UI over the Tauri boundary. This module only RENDERS a given fingerprint; it never re-derives
//    it. The cross-language agreement is pinned by `fingerprint_vectors.json` (see its test).

/** A locally-saved contact: a petname a user bound to a specific `npub` on follow. */
export interface Contact {
	npub: string;
	petname: string;
}

/** A word+color fingerprint as produced by the Rust helper and handed to the UI. */
export interface Fingerprint {
	words: string[];
	colorHex: string;
}

/** How a name should be shown beside a key. */
export interface DisplayLabel {
	/** The text to show (petname when known, else the teaser's display name). */
	label: string;
	/** True only when the key is a saved contact (petname bound to this exact npub). */
	verified: boolean;
	/** Set when a stranger reuses a contact's name under a different key (impersonation alert). */
	warning?: string;
	/** True when the key is neither a contact nor a flagged impersonator (an unverified stranger). */
	stranger: boolean;
}

/**
 * Resolve how to display `displayName` for `npub` against the local contacts.
 *  - exact npub match → the petname, verified;
 *  - a contact's name reused under a DIFFERENT key → the name, flagged "not <petname> — different key";
 *  - otherwise → the name, labelled an unverified stranger.
 */
export function petnameFor(npub: string, displayName: string, contacts: Contact[]): DisplayLabel {
	const exact = contacts.find((c) => c.npub === npub);
	if (exact) {
		return { label: exact.petname, verified: true, stranger: false };
	}
	const nameCollision = contacts.find((c) => c.petname === displayName && c.npub !== npub);
	if (nameCollision) {
		return {
			label: displayName,
			verified: false,
			stranger: false,
			warning: `not ${nameCollision.petname} — different key`,
		};
	}
	return { label: displayName, verified: false, stranger: true };
}

/** A short, stable label for the unverified-stranger badge a tag-search hit carries until followed. */
export function strangerBadge(label: DisplayLabel): string | null {
	return label.stranger ? 'unverified — not in your contacts' : null;
}

/**
 * Render a (Rust-derived) fingerprint to its at-a-glance display string. Deterministic: the same
 * fingerprint always renders the same way, and two distinct fingerprints render distinctly — that
 * is what makes the swatch a usable distinguisher. This RENDERS, it does not derive.
 */
export function renderFingerprint(fp: Fingerprint): string {
	return `${fp.words.join(' ')} ${fp.colorHex.toLowerCase()}`;
}
