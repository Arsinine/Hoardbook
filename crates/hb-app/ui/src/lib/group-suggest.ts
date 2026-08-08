// Group-name suggestions from cached profile data (M22 W2) — pure local computation over the
// profiles of the contacts just dropped together. No fetch, no relay traffic, no DOM. Mirrors the
// style of contacts-view.ts / browse-view.ts. The caller must NOT pre-fill the name field from the
// return — these are suggestions for the user to accept or discard, never a default (see the
// "never_a_default_pre_fill" test).

import type { CachedPeer } from './types.js';
import { contactSortName } from './contacts-view.js';

/** A trimmed value that is non-empty after trimming, else null (treat whitespace-only as absent). */
function clean(v: string | undefined): string | null {
	const t = v?.trim();
	return t ? t : null;
}

/** Case-folded de-duplication key, so "Anime" and "anime" don't both appear. Preserves first-seen
 *  casing for the rendered string. */
function dedupeCaseInsensitively(values: string[]): string[] {
	const seen = new Set<string>();
	const out: string[] = [];
	for (const v of values) {
		const key = v.toLowerCase();
		if (!seen.has(key)) {
			seen.add(key);
			out.push(v);
		}
	}
	return out;
}

/** Values present in every peer's field (after trim + case-fold), in first-peer order. `[]` when
 *  any peer lacks the field or the field is empty — intersection needs all peers to contribute. */
function intersectAcrossPeers(peers: CachedPeer[], field: 'tags' | 'content_types' | 'languages'): string[] {
	const perPeer = peers.map((p) => (p.profile?.[field] ?? []).map((v) => v.trim()).filter((v) => v.length > 0));
	if (perPeer.some((xs) => xs.length === 0)) return []; // any peer without the field ⇒ no intersection
	let common = perPeer[0];
	for (const xs of perPeer.slice(1)) {
		const lower = new Set(xs.map((v) => v.toLowerCase()));
		common = common.filter((v) => lower.has(v.toLowerCase()));
	}
	return common.length > 0 ? common : [];
}

/** The location case: only when every peer has the same non-empty location (string equality on the
 *  trimmed value, case-insensitive). Nothing is geocoded and no location leaves the machine. */
function sharedLocation(peers: CachedPeer[]): string | null {
	const locs = peers.map((p) => clean(p.profile?.location));
	if (locs.some((l) => l === null)) return null; // any peer without location ⇒ no geography suggestion
	const first = locs[0]!;
	return locs.every((l) => l!.toLowerCase() === first.toLowerCase()) ? first : null;
}

/** The petname fallback that always trails the list so it is never empty: "Marisol & Kestrel" for
 *  two peers, "Marisol +2" for three or more. A peer with no usable name (neither petname nor
 *  display_name) is skipped for the lead name but still counted in the "+N". */
function petnameFallback(peers: CachedPeer[]): string {
	const names = peers.map((p) => contactSortName(p)).filter((n): n is string => n !== null);
	if (peers.length <= 2) {
		// Two named peers → "A & B"; one or both nameless → join what we have (possibly just npub-less count).
		return names.length === peers.length ? names.join(' & ') : names.length > 0 ? names.join(' & ') : 'New group';
	}
	// 3+: lead with the first named peer and "+N" for the rest (by input order, deterministic).
	const lead = names[0] ?? 'New group';
	return `${lead} +${peers.length - 1}`;
}

/**
 * Suggest up to three group names from the cached profiles of the peers just dropped together, in
 * priority order: shared interest tags → shared content types → shared location → shared languages
 * (the last only when nothing above matched), then an always-present petname fallback as the final
 * entry. Returns suggestions, never a default — the caller must not pre-fill the field.
 */
export function suggestGroupNames(peers: CachedPeer[]): string[] {
	if (peers.length === 0) return [];

	// Priority sources 1–3. Languages (4) is only consulted when these produced nothing.
	const candidates: string[] = [];
	candidates.push(...intersectAcrossPeers(peers, 'tags'));
	candidates.push(...intersectAcrossPeers(peers, 'content_types'));
	const loc = sharedLocation(peers);
	if (loc) candidates.push(loc);

	// Source 4 — weakest signal, only when no higher-priority source matched.
	if (candidates.length === 0) {
		candidates.push(...intersectAcrossPeers(peers, 'languages'));
	}

	const unique = dedupeCaseInsensitively(candidates).slice(0, 2); // leave room for the fallback
	unique.push(petnameFallback(peers));
	return unique;
}
