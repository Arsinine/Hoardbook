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
 *  (the full-listing file a hoarder hands over via Mascara for a large truncated collection). */
export type ExportFormat = 'text' | 'markdown' | 'manifest';

export type RowMenuItem =
	| { key: 'rescan' | 'edit' | 'publish' | 'unpublish' | 'remove'; label: string }
	| { key: 'export'; label: string; submenu: { key: ExportFormat; label: string }[] };

/** The overflow-menu items for a row, in display order. Publish/Unpublish is mutually exclusive by
 *  published state. */
export function menuItems(col: Pick<Collection, 'published'>): RowMenuItem[] {
	return [
		{ key: 'rescan', label: 'Rescan' },
		{ key: 'edit', label: 'Edit details' },
		{
			key: 'export',
			label: 'Export',
			submenu: [
				{ key: 'text', label: 'Plain text' },
				{ key: 'markdown', label: 'Markdown checklist' },
				{ key: 'manifest', label: 'Manifest file (.hbmanifest)' },
			],
		},
		col.published ? { key: 'unpublish', label: 'Unpublish' } : { key: 'publish', label: 'Publish' },
		{ key: 'remove', label: 'Remove' },
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
