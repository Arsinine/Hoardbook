// M9 Track L — the **standalone** recursive-tree fixture for the L4 profiler (F20). Owned by M9, not
// coupled to M8's `ScanTreeNode`/`scan-tree` internals, so an M8 refactor can't break the profiler.
// It feeds the recursive-tree workload the heavy components (DirItem / CollectionPanel /
// ScanTreeNode) share — the surface where "excessive CPU/memory" regressions live.

import type { DirectoryItem } from '../types';

/**
 * Build a deterministic balanced tree with ~`nodeCount` total nodes and at most `depth` levels. No
 * RNG (so two runs produce byte-identical trees → stable profiler numbers). Folders branch until the
 * node budget is spent; leaves are files with a size string.
 */
export function makeTreeFixture(nodeCount: number, depth: number): DirectoryItem[] {
	const d = Math.max(1, depth);
	// Branch factor that reaches ~nodeCount nodes in `d` levels.
	const branching = Math.max(2, Math.ceil(Math.pow(Math.max(nodeCount, 2), 1 / d)));
	let remaining = nodeCount;
	let counter = 0;

	function build(level: number): DirectoryItem[] {
		const out: DirectoryItem[] = [];
		for (let i = 0; i < branching && remaining > 0; i++) {
			remaining--;
			const id = counter++;
			if (level < d - 1 && remaining > 0) {
				out.push({
					name: `folder-${id}`,
					item_type: 'Folder',
					tags: [],
					children: build(level + 1),
				});
			} else {
				out.push({
					name: `file-${id}.mkv`,
					item_type: 'File',
					size: `${(id % 97) + 1} MB`,
					format: 'MKV',
					tags: [],
					children: [],
				});
			}
		}
		return out;
	}

	return build(0);
}

/** Count total nodes (files + folders) in a tree. */
export function countNodes(items: DirectoryItem[]): number {
	return items.reduce((n, it) => n + 1 + countNodes(it.children), 0);
}

/** Max depth of a tree (1 for a flat list). */
export function treeDepth(items: DirectoryItem[]): number {
	let max = 0;
	for (const it of items) {
		if (it.children.length > 0) max = Math.max(max, treeDepth(it.children));
	}
	return items.length > 0 ? max + 1 : 0;
}
