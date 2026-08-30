// Contacts phonebook view-model (devtest #17/#18 redesign) — pure sort/group/filter logic for the
// A-Z rail + Groups view, unit-tested without a DOM so the Svelte page stays thin. Mirrors the style
// of browse-view.ts / chat-filter.ts / contact-display.ts.

import type { CachedPeer, Group } from './types.js';

/** One rendered section of the phonebook — an A-Z letter, the "Ungrouped" bucket, or a named group. */
export interface PhonebookSection {
	key: string;
	label: string;
	anchorId: string;
	peers: CachedPeer[];
}

/** A-Z rail order: every ASCII letter, then the nameless/non-ASCII catch-all. */
export const ALPHABET: readonly string[] = [
	...Array.from({ length: 26 }, (_, i) => String.fromCharCode(65 + i)),
	'#',
];

/** The name to sort/group a contact by: local petname first, then their published display_name —
 *  **never** the npub. Whitespace-only values are treated as absent. `null` when the peer has
 *  neither (the nameless residual, sorted under the trailing "#" by npub — see `sortKeyForPeer`). */
export function contactSortName(peer: { petname?: string; profile?: { display_name?: string } }): string | null {
	const petname = peer.petname?.trim();
	if (petname) return petname;
	const displayName = peer.profile?.display_name?.trim();
	if (displayName) return displayName;
	return null;
}

/** Sort key for a contact: every named peer sorts before every nameless one (case-insensitive name
 *  compare); nameless peers sort among themselves by npub. */
export function sortKeyForPeer(peer: { npub: string; petname?: string; profile?: { display_name?: string } }): string {
	const name = contactSortName(peer);
	if (name) return `0:${name.toLowerCase()}`;
	return `1:${peer.npub.toLowerCase()}`;
}

function sortPeers(peers: CachedPeer[]): CachedPeer[] {
	return [...peers].sort((a, b) => sortKeyForPeer(a).localeCompare(sortKeyForPeer(b)));
}

/** The A-Z rail letter a contact belongs under: the uppercased first character of its sort-name when
 *  that's an ASCII A-Z letter, else the "#" catch-all (non-ASCII, digit, punctuation, or nameless). */
export function letterForPeer(peer: { npub: string; petname?: string; profile?: { display_name?: string } }): string {
	const name = contactSortName(peer);
	if (!name) return '#';
	const first = name.trim()[0]?.toUpperCase() ?? '';
	return first >= 'A' && first <= 'Z' ? first : '#';
}

/** Group contacts by A-Z rail letter: sections in A..Z then "#" order, empty letters omitted, each
 *  section's peers sorted. Anchor ids are `sec-A`..`sec-Z`, `sec-hash`. */
export function groupByLetter(peers: CachedPeer[]): PhonebookSection[] {
	const buckets = new Map<string, CachedPeer[]>();
	for (const p of peers) {
		const letter = letterForPeer(p);
		const bucket = buckets.get(letter);
		if (bucket) bucket.push(p);
		else buckets.set(letter, [p]);
	}
	const sections: PhonebookSection[] = [];
	for (const letter of ALPHABET) {
		const members = buckets.get(letter);
		if (!members || members.length === 0) continue;
		sections.push({
			key: letter,
			label: letter,
			anchorId: letter === '#' ? 'sec-hash' : `sec-${letter}`,
			peers: sortPeers(members),
		});
	}
	return sections;
}

/** Group contacts by the user's own groups (M13 groups): one section per group with ≥1 visible
 *  member, in the caller's group order (multi-membership — a peer in two groups appears in both
 *  sections), followed by a trailing "Ungrouped" section for peers in none of `groups`. A group with
 *  zero visible members is omitted entirely. Group anchors are `sec-grp-<index>` keyed to the
 *  group's position in the input `groups` array; Ungrouped is `sec-grp-ungrouped`. */
export function groupByGroups(peers: CachedPeer[], groups: Group[]): PhonebookSection[] {
	const sections: PhonebookSection[] = [];
	const groupedNpubs = new Set<string>();
	groups.forEach((g, index) => {
		const members = peers.filter((p) => g.pubkeys.includes(p.npub));
		if (members.length === 0) return;
		for (const m of members) groupedNpubs.add(m.npub);
		sections.push({ key: g.name, label: g.name, anchorId: `sec-grp-${index}`, peers: sortPeers(members) });
	});
	const ungrouped = peers.filter((p) => !groupedNpubs.has(p.npub));
	if (ungrouped.length > 0) {
		sections.push({ key: 'ungrouped', label: 'Ungrouped', anchorId: 'sec-grp-ungrouped', peers: sortPeers(ungrouped) });
	}
	return sections;
}

/** The pinned "● Online now" bucket — every online peer, sorted. `[]` when nobody is online. In the
 *  Name view an online peer is shown here *instead of* its A-Z section (devtest #1 — the page filters
 *  them out of `groupByLetter`); the Groups view drops this bucket entirely (devtest #8). */
export function onlineBucket(peers: CachedPeer[]): CachedPeer[] {
	return sortPeers(peers.filter((p) => p.online === true));
}

/** The per-profile sub-haystack — the ONE place the published half of `matchesQuery` reads from.
 *  Kept next to the page's `hasProfileFacts` shape deliberately: every field the contact DETAIL
 *  CARD renders is searched here, so a field added to the card has one obvious place to land.
 *  QURATOR-136: `profile.tags` was missing, so locally tagging a contact made them findable but a
 *  tag THEY published silently matched nothing. The card renders since/est_size/picture/updated
 *  too, but those are a year/size-label/avatar/timestamp — not things you type into a search box,
 *  so they stay out (see contacts-view.test.ts for the guard that keeps this list honest). */
function profileHaystack(profile: CachedPeer['profile'] | null | undefined): string[] {
	if (!profile) return [];
	return [
		profile.display_name,
		profile.bio ?? '',
		...(profile.tags ?? []),
		...(profile.content_types ?? []),
		...(profile.languages ?? []),
		...(profile.willing_to ?? []),
		profile.location ?? '',
		profile.contact_hint ?? '',
		// Social handles, not platform labels ("github"/"matrix" are enums — searching them would
		// return everyone with a GitHub link, which is not what anyone types).
		...(profile.social_links ?? []).map((sl) => sl.handle),
	];
}

/** Case-insensitive free-text search across every field a contact card actually shows: display_name,
 *  petname, npub, bio, their published tags/languages/willing-to/region/contact hint (QURATOR-136 —
 *  the detail card renders these), your local tags, content types, and each collection's
 *  path_alias/description/content_types. `profile.email` is deliberately NOT searched: the owner
 *  opted it in for hand-copying from the card, and the search box is not the place to enumerate
 *  who has an address on file. Empty/whitespace query matches everything (identity). Never throws
 *  on an absent profile or empty collections list. */
export function matchesQuery(peer: CachedPeer, q: string): boolean {
	const query = q.trim().toLowerCase();
	if (!query) return true;
	const haystacks: string[] = [
		peer.petname ?? '',
		peer.npub,
		...(peer.local_tags ?? []),
		...profileHaystack(peer.profile),
	];
	for (const col of peer.collections ?? []) {
		haystacks.push(col.path_alias ?? '', col.description ?? '', ...(col.content_types ?? []));
	}
	return haystacks.some((h) => h.toLowerCase().includes(query));
}

/** The set of section keys actually present — drives which A-Z rail buttons are enabled. */
export function presentSectionKeys(sections: PhonebookSection[]): Set<string> {
	return new Set(sections.map((s) => s.key));
}
