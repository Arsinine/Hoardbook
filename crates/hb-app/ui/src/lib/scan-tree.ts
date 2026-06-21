// Folder-tree picker derivation (M8, HANDOVER §A2.1) — pure include-set + tri-state logic, unit
// tested with vitest so ScanTreeNode/ScanDialog stay thin. Mirrors the backend `IncludeSet`
// (hb-app::commands::collection) so the picker and the selective walk agree on what gets published.
//
// `rel` paths are relative, "/"-separated directory paths (no leading/trailing slash), exactly the
// strings sent to the backend as `ScanOptions.include`.

/** `rel` is included iff it is itself checked or lives under a checked ancestor. */
export function isIncluded(rel: string, checked: Set<string>): boolean {
	if (checked.has(rel)) return true;
	for (const c of checked) {
		if (rel.startsWith(c + '/')) return true;
	}
	return false;
}

/** True iff some checked path lives strictly below `rel` (so `rel` is only an *ancestor* of a
 *  selection — its checkbox renders indeterminate). */
export function hasDescendantUnder(rel: string, checked: Set<string>): boolean {
	const prefix = rel + '/';
	for (const c of checked) {
		if (c.startsWith(prefix)) return true;
	}
	return false;
}

export type TriState = 'checked' | 'locked' | 'indeterminate' | 'unchecked';

/**
 * The checkbox display state for `rel`, in strict precedence order (F7 — total, not ambiguous):
 *   1. explicitly checked            → 'checked'
 *   2. included via a checked ancestor (not itself checked) → 'locked' (uncheck the parent to refine)
 *   3. has a checked descendant, not itself included        → 'indeterminate'
 *   4. otherwise                                            → 'unchecked'
 * Locked (ancestor) deliberately beats indeterminate (descendant): if an ancestor already pulls the
 * whole subtree in, the node is fully selected regardless of any deeper explicit check.
 */
export function triState(rel: string, checked: Set<string>): TriState {
	if (checked.has(rel)) return 'checked';
	if (isIncluded(rel, checked)) return 'locked';
	if (hasDescendantUnder(rel, checked)) return 'indeterminate';
	return 'unchecked';
}

/**
 * The `include` array to send to the backend: the checked set with redundant deep checks dropped —
 * a node whose ancestor is also checked adds nothing (the ancestor walks it fully), so shipping both
 * `"a"` and `"a/b"` is removed to just `["a"]`.
 */
export function serializeInclude(checked: Set<string>): string[] {
	const all = [...checked];
	return all.filter((c) => !all.some((d) => d !== c && c.startsWith(d + '/')));
}

/** "Select all" — check every top-level node by name. */
export function selectAllTopLevel(topLevelRels: string[]): Set<string> {
	return new Set(topLevelRels);
}
