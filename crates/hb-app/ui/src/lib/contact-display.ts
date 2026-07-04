// Pure view-model for how a contact's name is rendered (M13 W5 Slice 2). Petname-first (the user's
// own local, impersonation-resistant nickname), then the peer's published display_name, then a short
// npub — never a blank or a bare "Unknown". No Svelte, no DOM.

/** The minimal shape needed to resolve a display name — a `CachedPeer` satisfies this structurally. */
export interface ContactNameSource {
	npub: string;
	petname?: string;
	profile?: { display_name?: string } | null;
}

/** Truncate a long npub to `npub1abcd…wxyz`; short ones pass through unchanged. */
export function shortNpub(npub: string): string {
	return npub.length > 14 ? `${npub.slice(0, 8)}…${npub.slice(-4)}` : npub;
}

/** The name to show for a contact: local petname first, then their published display_name, then a
 *  short npub. Whitespace-only values are treated as absent. */
export function contactDisplayName(peer: ContactNameSource): string {
	const petname = peer.petname?.trim();
	if (petname) return petname;
	const displayName = peer.profile?.display_name?.trim();
	if (displayName) return displayName;
	return shortNpub(peer.npub);
}
