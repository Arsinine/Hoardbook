// Browse view-model (M3) — pure rendering helpers for the relay-read browse UI, unit-tested with
// vitest so the Svelte components stay thin. Mirrors the shape `hb-net`'s browse orchestration
// returns: a (possibly partial) rendered listing and tag-search hits.

/** A node in a decrypted listing tree (a folder or file). */
export interface TreeNode {
	name: string;
	children?: TreeNode[];
}

/** A flattened row for list rendering. */
export interface TreeRow {
	name: string;
	depth: number;
}

/** Mirrors `hb_net::RenderedListing`: the tree plus the K-of-N availability the relay returned. */
export interface RenderedListing {
	entries: TreeNode[];
	partsTotal: number;
	partsPresent: number;
	missing: number[];
}

/**
 * A tag-search hit — the VIEW projection of `hb_net::SearchHit { npub, teaser }`: the Tauri bridge
 * maps `teaser.display_name` → `displayName`. Deliberately carries NO listing/tree: a search hit is
 * teaser-only and the listing needs the share code (DISC3).
 */
export interface SearchHit {
	npub: string;
	displayName: string;
}

/**
 * Flatten a nested listing tree into depth-tagged rows, depth-first, preserving order. Iterative
 * (an explicit stack) so a deeply-nested or very wide listing can't overflow the call stack or hit
 * the spread-operator argument limit.
 */
export function flattenTree(entries: TreeNode[]): TreeRow[] {
	const rows: TreeRow[] = [];
	// Stack of nodes to visit, each with its depth; seed in reverse so siblings emit in order.
	const stack: Array<{ node: TreeNode; depth: number }> = [];
	for (let i = entries.length - 1; i >= 0; i--) {
		stack.push({ node: entries[i], depth: 0 });
	}
	while (stack.length > 0) {
		const { node, depth } = stack.pop()!;
		rows.push({ name: node.name, depth });
		if (node.children) {
			for (let i = node.children.length - 1; i >= 0; i--) {
				stack.push({ node: node.children[i], depth: depth + 1 });
			}
		}
	}
	return rows;
}

/** The "K of N folders available" badge for a partial listing; `null` when complete. */
export function availabilityBadge(listing: RenderedListing): string | null {
	if (listing.partsPresent >= listing.partsTotal) {
		return null;
	}
	return `${listing.partsPresent} of ${listing.partsTotal} folders available`;
}

/**
 * The "K of N folders available" badge for a peer `Collection` carrying K-of-N part counts (M13
 * HANDOVER gap #5) — the browsed-peer sibling of `availabilityBadge`, which reads a raw
 * `hb_net::RenderedListing`. `null` when either count is absent (a pre-M13 cached peer collection
 * must never show a fabricated badge) or when the collection is complete.
 */
export function collectionAvailability(col: { parts_total?: number; parts_present?: number }): string | null {
	if (col.parts_total === undefined || col.parts_present === undefined) {
		return null;
	}
	if (col.parts_present >= col.parts_total) {
		return null;
	}
	return `${col.parts_present} of ${col.parts_total} folders available`;
}

/** Dedup search hits by npub (keep first), then cap to `limit` — mirrors the client-side guard. */
export function dedupAndCap(hits: SearchHit[], limit: number): SearchHit[] {
	const seen = new Set<string>();
	const out: SearchHit[] = [];
	for (const hit of hits) {
		if (seen.has(hit.npub)) continue;
		seen.add(hit.npub);
		out.push(hit);
		if (out.length >= limit) break;
	}
	return out;
}
