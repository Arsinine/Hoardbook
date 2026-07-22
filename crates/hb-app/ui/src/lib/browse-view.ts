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

/** A contact row's at-a-glance browse-key status (devtest #1) — whether their listings can actually
 *  be decrypted, surfaced on the row itself instead of only after selecting them. */
export interface PeerAccessBadge {
	locked: boolean;
	icon: string;
	label: string;
	hint: string;
}

/** Keyed off `browse_key_hex` alone — **never** collection count. A bare contact whose cache still
 *  carries stale collections (e.g. from before the key was lost) must still read as locked. */
export function peerAccessBadge(peer: { browse_key_hex?: string }): PeerAccessBadge {
	if (peer.browse_key_hex) {
		return { locked: false, icon: '🔓', label: 'browseable', hint: '' };
	}
	return {
		locked: true,
		icon: '🔒',
		label: 'key needed',
		hint: 'Ask them for their share code to browse their collections.',
	};
}

/** Count every node (file + folder, recursively) in a listing tree. devtest #7 — used to show how
 *  many items a truncated paywall teaser is hiding: `total_items − countListingItems(listing)`. */
export function countListingItems(items: readonly unknown[]): number {
	let n = 0;
	for (const it of items) {
		n += 1;
		const kids = (it as { children?: readonly unknown[] }).children;
		if (Array.isArray(kids)) n += countListingItems(kids);
	}
	return n;
}

/**
 * The paywall-teaser summary for a browsed collection, or `null` when there is no teaser to show
 * (devtest #7 / M16 W3). `null` for a **non-truncated** collection — one published whole, OR a
 * truncated one the browser upgraded to the full tree from a big relay (M16 W3 clears `truncated`
 * on a successful upgrade); either way the full tree renders with no fade. Also `null` when nothing
 * is actually hidden (`shown >= total`). Pure — the Svelte component ANDs the top-level-view guard
 * (no paywall while drilled into a subfolder, where the dropped tail wouldn't make the fade honest).
 */
export function paywallTeaser(
	col: { truncated?: boolean; total_items?: number; listing?: readonly unknown[] } | null | undefined,
): { shown: number; hidden: number; total: number } | null {
	if (!col?.truncated || !col.total_items) return null;
	const shown = countListingItems(col.listing ?? []);
	const hidden = Math.max(0, col.total_items - shown);
	return hidden > 0 ? { shown, hidden, total: col.total_items } : null;
}

/** M16 W4 — the "Full manifest imported · <date>" tag shown once the user has imported the full-listing
 *  manifest of a truncated collection (its fade lifts, `paywallTeaser` goes `null`). `null` for a
 *  normally-browsed collection. Pure. */
export function importedManifestNote(
	col: { manifest_imported_at?: number } | null | undefined,
): string | null {
	if (!col?.manifest_imported_at) return null;
	const when = new Date(col.manifest_imported_at * 1000).toLocaleDateString();
	return `Full manifest imported · ${when}`;
}

/** Byte-size units, largest first, for `parseEstSize`/`summarizeCollectionsSize` (devtest #7). */
const SIZE_UNITS: Array<[string, number]> = [
	['TB', 1024 ** 4],
	['GB', 1024 ** 3],
	['MB', 1024 ** 2],
	['KB', 1024],
	['B', 1],
];

/** Parse a formatted size string (e.g. "14.2 GB", "~12 TB") into bytes. Tolerant of a missing space
 *  and a leading "~"; an absent or unparseable string yields `0` (never fabricate a size). */
export function parseEstSize(s: string | undefined): number {
	if (!s) return 0;
	const m = /^\s*~?\s*([\d.]+)\s*(B|KB|MB|GB|TB)\b/i.exec(s);
	if (!m) return 0;
	const value = parseFloat(m[1]);
	if (Number.isNaN(value)) return 0;
	const unit = SIZE_UNITS.find(([name]) => name === m[2].toUpperCase());
	return unit ? value * unit[1] : 0;
}

/** Format a byte count in its largest whole unit (1 decimal place), e.g. `536870912` → `"512.0 MB"`. */
function fmtLargestUnit(bytes: number): string {
	for (const [unit, size] of SIZE_UNITS) {
		if (bytes >= size) return `${(bytes / size).toFixed(1)} ${unit}`;
	}
	return `${bytes.toFixed(1)} B`;
}

/** The "~X across N collections" summary for a keyed peer's collections (devtest #7) — sums each
 *  collection's self-declared `est_size` (never fabricates from anything else; the wire carries no
 *  teaser scale field). `M` counts every collection passed in, even an unparseable one; `null` when
 *  the total is zero (nothing parseable — never render a fabricated "0 B"). */
export function summarizeCollectionsSize(cols: { est_size?: string }[]): string | null {
	const totalBytes = cols.reduce((sum, c) => sum + parseEstSize(c.est_size), 0);
	if (totalBytes <= 0) return null;
	const m = cols.length;
	return `~${fmtLargestUnit(totalBytes)} across ${m} collection${m !== 1 ? 's' : ''}`;
}

/** M15 W4 — resolve a `/browse?peer=<npub>` deep-link param against the loaded contacts. Returns the
 *  matching contact, or null for an absent/unknown param (caller stays on the empty state). The
 *  caller feeds the result through Browse's own `selectPeer`, so the keyed-contact live-refetch
 *  (devtest #3/#4) is preserved by construction. */
export function peerFromQuery<P extends { npub: string }>(
	searchParams: URLSearchParams,
	contacts: readonly P[],
): P | null {
	const npub = searchParams.get('peer');
	if (!npub) return null;
	return contacts.find((c) => c.npub === npub) ?? null;
}

// ── devtest v0.12.4 #4 — the collection file view controls (view mode · type filter · sort · search).
//    Pure + vitest-covered so the Svelte component holds only toggle state + rendering. All operate on
//    the flat list of the CURRENT folder's items (metadata only — INV-4: no open/download affordance).

/** The minimal shape these helpers need from a `DirectoryItem` (structural, so the component passes
 *  real `DirectoryItem[]` and gets the same concrete type back through the generic). */
export interface ArrangeableItem {
	name: string;
	item_type: 'File' | 'Folder';
	size?: string;
	format?: string;
}

export type BrowseViewMode = 'details' | 'folders';
export type BrowseSortKey = 'name' | 'size' | 'type';
export type BrowseSortDir = 'asc' | 'desc';

/** Distinct file formats (types) present among `items`, normalized (trimmed + lowercased) and sorted —
 *  the source for the type-filter chips. Folders and format-less files contribute nothing, so an
 *  all-folders view shows no chips. */
export function fileTypesPresent(items: readonly ArrangeableItem[]): string[] {
	const set = new Set<string>();
	for (const it of items) {
		if (it.item_type !== 'File') continue;
		const f = it.format?.trim().toLowerCase();
		if (f) set.add(f);
	}
	return [...set].sort();
}

/** Controls for {@link arrangeItems}. `types` empty ⇒ no type filter (show every item). */
export interface ArrangeOptions {
	search?: string;
	types?: readonly string[];
	sortKey?: BrowseSortKey;
	sortDir?: BrowseSortDir;
}

/**
 * Filter + sort a folder's items for display (pure — never mutates the input):
 * - `search`: case-insensitive name substring, applied to BOTH files and folders;
 * - `types`: when non-empty, keep only **File** items whose `format` ∈ `types`. **Folders are always
 *   kept** so drill-down never breaks (a folder may hold matching files deeper);
 * - sort: **folders always precede files** (the existing convention, independent of `sortDir`), then
 *   by `sortKey`/`sortDir` within each group. `size` sorts by parsed bytes (`parseEstSize`), `type` by
 *   `format`; both fall back to name so the order is stable and predictable.
 */
export function arrangeItems<T extends ArrangeableItem>(items: readonly T[], opts: ArrangeOptions = {}): T[] {
	const search = (opts.search ?? '').trim().toLowerCase();
	const types = opts.types ?? [];
	const sortKey = opts.sortKey ?? 'name';
	const dir = opts.sortDir === 'desc' ? -1 : 1;

	const filtered = items.filter((it) => {
		if (search && !it.name.toLowerCase().includes(search)) return false;
		if (types.length > 0 && it.item_type === 'File') {
			return types.includes(it.format?.trim().toLowerCase() ?? '');
		}
		return true;
	});

	return [...filtered].sort((a, b) => {
		// Folders before files, ALWAYS — this grouping is not flipped by sortDir.
		if (a.item_type !== b.item_type) return a.item_type === 'Folder' ? -1 : 1;
		let primary: number;
		if (sortKey === 'size') primary = parseEstSize(a.size) - parseEstSize(b.size);
		// Compare type case-insensitively, matching the lowercased filter chips (fileTypesPresent).
		else if (sortKey === 'type') primary = (a.format ?? '').toLowerCase().localeCompare((b.format ?? '').toLowerCase());
		else primary = a.name.localeCompare(b.name);
		if (primary !== 0) return primary * dir;
		// Equal primary → a stable ASCENDING name tiebreak, never flipped by sortDir.
		return a.name.localeCompare(b.name);
	});
}
