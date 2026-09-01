// Pure view-model for the compact collection row (M13 W5 Slice 1 — replaces the old always-on
// accordion editor). No Svelte, no DOM. A collection is publish-ready the moment it exists (the
// content-type gate lives in the Add-collection wizard's Details step, not here).

import type { Collection } from './types.js';

export type RowChip = 'Draft' | 'Published';

/** Draft until published — mirrors the old inline draft-badge, now the row's status chip. */
export function deriveRowChip(col: Pick<Collection, 'published'>): RowChip {
	return col.published ? 'Published' : 'Draft';
}

/** Export sub-actions: the two human-readable checklists, plus the M16 W4 `.hbmanifest` envelope
 *  (the full-listing file a hoarder hands over themselves for a large truncated collection — the
 *  fallback route since M18 W4 gave Chat a "Send the full list" verb over the transport plane). */
export type ExportFormat = 'text' | 'markdown' | 'manifest';

export type RowMenuItem =
	| { key: 'rescan' | 'edit' | 'publish' | 'remove'; label: string }
	| { key: 'export'; label: string; submenu: { key: ExportFormat; label: string }[] };

/** The overflow-menu items for a row, in display order. There is no Unpublish — QURATOR-138
 *  (owner ruling 2026-08-30): "Unpublish becomes DELETE. One destructive operation that removes
 *  the local record and zeroes the published event." Delete is the single retract+remove
 *  affordance, confirmed before it runs (INV-8). The `published` flag now picks only the Delete
 *  confirm copy (retraction wording for published rows) — see CollectionRow.svelte. */
export function menuItems(col: Pick<Collection, 'published'>): RowMenuItem[] {
	// QURATOR-138: the Export entry (and its text/markdown/manifest submenu) is deleted — owner:
	// "Delete the … export buttons in collections as well."
	return [
		{ key: 'rescan', label: 'Rescan' },
		{ key: 'edit', label: 'Edit details' },
		{ key: 'publish', label: 'Publish' },
		{ key: 'remove', label: 'Delete' },
	];
}

export interface RowBadge {
	label: string;
	kind: 'sorted' | 'private';
}

/** Sorted/Private badges shown on the row — omit whichever isn't set. Absent visibility ⇒ Public
 *  (a pre-M10 collection), so it never renders a silent Private badge. */
export function badges(col: Pick<Collection, 'sorted' | 'visibility'>): RowBadge[] {
	const out: RowBadge[] = [];
	if (col.sorted) out.push({ label: 'Sorted', kind: 'sorted' });
	if ((col.visibility ?? 'Public') === 'Private') out.push({ label: 'Private', kind: 'private' });
	return out;
}

// ── Type icon (QURATOR-140) ───────────────────────────────────────────────────────────────────────
// A type icon means the collection IS that thing, so it is earned only by a PURE collection — one
// single content type, from the fixed six-value enum the details form offers
// (video/audio/image/text/software/other). Owner ruling 2026-08-27: "a video icon should be for a
// pure video collection."
//
// Anything mixed falls to the folder, which is why the folder stays meaningful: it says "not one
// thing" (mixed, Other, or not yet classified). Keying off content_types[0] instead would let a
// video+audio collection wear the video icon and assert something false about its contents — the
// user picks these in a form, so [0] is the first box they happened to tick, not a dominant type.
export type RowIcon = 'video' | 'audio' | 'image' | 'text' | 'software' | 'folder';

export function rowIcon(col: Pick<Collection, 'content_types'>): RowIcon {
	const types = col.content_types ?? [];
	if (types.length !== 1) return 'folder'; // mixed, or none set yet — never one thing
	switch (types[0]) {
		case 'video': case 'audio': case 'image': case 'text': case 'software': return types[0];
		default: return 'folder'; // 'other', or an unrecognised value
	}
}

// ── Size tiers (devtest 2026-08-26 item 6) ────────────────────────────────────────────────────────
// The owner asked for size-coded item counts: "under 80000 items remain as is. 80000-99999 items use
// amber, 100000 items and over use faint red along with a tooltip warning."
//
// The 100_000 boundary is NOT a free choice — it is MAX_COLLECTION_ITEMS, enforced in Rust by
// `enforce_item_cap` in hb-app/src/commands/collection.rs. A collection at or over it cannot be
// scanned, so "faint red" is warning the user about a real, already-enforced wall; amber is the
// approach warning below it. The two constants are pinned against the Rust source by
// collection-row-view.test.ts so the tiers can never silently drift off the cap they describe.
export const MAX_COLLECTION_ITEMS = 100_000;
export const ITEM_COUNT_WARN_AT = 80_000;

export type SizeTier = 'normal' | 'warn' | 'over';

/** Which tier an item count falls in. Pure — the single source both the row and the panel use. */
export function sizeTier(itemCount: number): SizeTier {
	if (itemCount >= MAX_COLLECTION_ITEMS) return 'over';
	if (itemCount >= ITEM_COUNT_WARN_AT) return 'warn';
	return 'normal';
}

/** The tooltip for a tier, or null when there is nothing to warn about (tier `normal` stays bare —
 *  "remain as is"). Only the over-cap tier was asked to carry a tooltip; amber gets one too because
 *  a bare colour change with no explanation is unreadable, and it is the same warning one step earlier. */
export function sizeTierTooltip(itemCount: number): string | null {
	switch (sizeTier(itemCount)) {
		case 'over':
			// Careful with the boundary: Rust's `enforce_item_cap` rejects `item_count >
			// MAX_COLLECTION_ITEMS`, so a collection of EXACTLY 100,000 still scans fine (pinned by
			// `enforce_item_cap_is_inclusive_at_exactly_the_ceiling`). The owner asked for red at
			// "100000 items and over", so the TIER is right — but the tooltip must not tell a
			// 100,000-item collection it cannot be scanned, because it can. Phrased to hold at both
			// ends: true at the cap, true past it.
			return `${itemCount.toLocaleString()} items is at or past the ${MAX_COLLECTION_ITEMS.toLocaleString()}-item cap. Past the cap a collection can no longer be scanned or published. Split it into smaller collections.`;
		case 'warn':
			return `${itemCount.toLocaleString()} items is approaching the ${MAX_COLLECTION_ITEMS.toLocaleString()}-item cap. Past that, this collection can no longer be scanned or published.`;
		default:
			return null;
	}
}
