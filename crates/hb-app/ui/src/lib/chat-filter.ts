// Chat sidebar search (wires the existing dead search box — devtest copy audit). Pure filtering logic
// so the case-insensitive petname/display-name/npub-prefix matching is unit-tested without a DOM.

import type { CachedPeer, TopicView } from './types.js';

/** Case-insensitive match against a conversation peer's resolved display name (petname/display_name,
 *  via `nameOf`) OR its npub — so pasting a partial npub still finds the right row. Empty query = every
 *  peer, unchanged (identity). */
export function filterConversations(
	peers: CachedPeer[],
	query: string,
	nameOf: (npub: string) => string,
): CachedPeer[] {
	const q = query.trim().toLowerCase();
	if (!q) return peers;
	return peers.filter((p) => nameOf(p.npub).toLowerCase().includes(q) || p.npub.toLowerCase().includes(q));
}

/** Case-insensitive match against a Topic's name OR description (devtest #17/#18 sticky search
 *  widening — a Topic's roster petnames aren't part of `TopicView`, so name+description is the full
 *  searchable surface here). Empty query = every Topic, unchanged (identity). */
export function filterTopics(topics: TopicView[], query: string): TopicView[] {
	const q = query.trim().toLowerCase();
	if (!q) return topics;
	return topics.filter((t) => t.name.toLowerCase().includes(q) || t.description.toLowerCase().includes(q));
}

/** What kind of recipient a pasted compose-to string looks like — a PREFIX check only, for immediate
 *  UI feedback; the backend (`ShareCode::parse`) is the authoritative validator on send. */
export type RecipientKind = 'npub' | 'sharecode' | 'invalid';

export function composeRecipientKind(input: string): RecipientKind {
	const s = input.trim();
	if (s.startsWith('npub1')) return 'npub';
	if (s.startsWith('hbk1')) return 'sharecode';
	return 'invalid';
}

/** devtest #14 — a pasted-in npub or full share code that is unmistakably **your own** (an exact
 *  match; a partial/prefix paste still reaches the authoritative backend check on send, which
 *  decodes any share code). Drives the composer's disabled-Send + hint UX. */
export function isComposeToSelf(input: string, myNpub: string, myShareCode: string): boolean {
	const s = input.trim();
	if (!s) return false;
	return s === myNpub || s === myShareCode;
}
