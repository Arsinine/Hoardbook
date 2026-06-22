// M9 Track L — the L4 frontend profiler. Captures, per heavy surface, the cost of the recursive-tree
// processing the components share over M9's standalone fixture: total scripting time, peak heap
// delta, the longest single synchronous chunk (a "long task" proxy), and the node count.
//
// **Why a workload profiler, not a headless browser (justification, Open Q#6 / spec §Testing → L4):**
// the heavy components are lazy (DirItem/CollectionPanel/ScanTreeNode render collapsed) and
// ScanTreeNode calls a Tauri command on expand, so jsdom can't drive them into their heavy state, and
// jsdom can't produce real long-task entries or heap snapshots. The lightest option that yields
// **stable** numbers is to measure the recursive-tree workload these surfaces share — sort + recurse
// + flatten over a large tree — which is exactly the "excessive CPU/memory" regression surface (the
// frontend analog of the 2026-06-07 host-side loop spin). Real per-route DOM/CDP heap-snapshot +
// long-task profiling is the Playwright path, deferred to launch calibration (the one heavy browser
// tool); a smoke test proves the same fixture renders through a real component (F20).

import type { DirectoryItem } from '../types';
import { makeTreeFixture, countNodes } from './fixture';

export interface TargetMetrics {
	/** Total wall-clock scripting time across the measured iterations (ms). */
	scriptMs: number;
	/** The longest single synchronous iteration (ms) — a "long task" proxy. */
	longTaskMs: number;
	/** Peak JS heap delta across the workload (KB); 0 when not measurable in this runtime. */
	heapKb: number;
	/** Total nodes processed (deterministic — the stable anchor metric). */
	nodeCount: number;
}

export interface ProfileReport {
	targets: Record<string, TargetMetrics>;
}

/** A target the profiler exercises: name → the fixture (nodeCount, depth) it processes. */
interface TargetSpec {
	name: string;
	nodeCount: number;
	depth: number;
}

/** The heavy recursive-tree surfaces (corrected M9 targets: DownloadQueue was deleted in M7). */
const TARGETS: TargetSpec[] = [
	{ name: 'DirItem', nodeCount: 2000, depth: 5 }, // the recursive browse tree
	{ name: 'CollectionPanel', nodeCount: 2000, depth: 4 }, // a published listing tree
	{ name: 'ScanTreeNode', nodeCount: 2000, depth: 6 }, // the recursive folder-tree picker
];

const ITERATIONS = 20;

/**
 * The representative heavy operation the recursive components perform on a tree: a depth-first sort
 * (folders before files, then by name — what the browse view does) plus a recursive count. Returns
 * the number of nodes touched so the optimizer can't elide the work.
 */
function processTree(items: DirectoryItem[]): number {
	const sorted = [...items].sort((a, b) => {
		if (a.item_type !== b.item_type) return a.item_type === 'Folder' ? -1 : 1;
		return a.name.localeCompare(b.name);
	});
	let touched = sorted.length;
	for (const it of sorted) {
		if (it.children.length > 0) touched += processTree(it.children);
	}
	return touched;
}

function heapUsedKb(): number {
	// Available in Node (the profiler runtime); 0 elsewhere. We optionally trigger GC first if the
	// process was launched with --expose-gc, for a less noisy delta.
	const g = globalThis as unknown as { gc?: () => void; process?: { memoryUsage?: () => { heapUsed: number } } };
	if (typeof g.gc === 'function') g.gc();
	const mem = g.process?.memoryUsage?.();
	return mem ? Math.round(mem.heapUsed / 1024) : 0;
}

function profileTarget(spec: TargetSpec): TargetMetrics {
	const tree = makeTreeFixture(spec.nodeCount, spec.depth);
	const nodeCount = countNodes(tree);

	// Warm up so the first-run JIT cost doesn't skew the measured iterations.
	processTree(tree);

	const heapBefore = heapUsedKb();
	let total = 0;
	let longest = 0;
	let sink = 0;
	for (let i = 0; i < ITERATIONS; i++) {
		const t0 = performance.now();
		sink += processTree(tree);
		const dt = performance.now() - t0;
		total += dt;
		if (dt > longest) longest = dt;
	}
	const heapAfter = heapUsedKb();
	// Defeat dead-code elimination of `sink`.
	if (sink < 0) throw new Error('unreachable');

	return {
		scriptMs: round(total),
		longTaskMs: round(longest),
		heapKb: Math.max(0, heapAfter - heapBefore),
		nodeCount,
	};
}

function round(n: number): number {
	return Math.round(n * 1000) / 1000;
}

/** Run the full profile over every heavy target and return the report. */
export function runProfile(): ProfileReport {
	const targets: Record<string, TargetMetrics> = {};
	for (const spec of TARGETS) {
		targets[spec.name] = profileTarget(spec);
	}
	return { targets };
}
