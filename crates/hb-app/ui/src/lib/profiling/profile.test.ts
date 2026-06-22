// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { render } from '@testing-library/svelte';

import { makeTreeFixture, countNodes, treeDepth } from './fixture';
import { runProfile } from './profile';
import { checkBudgets, formatBreaches, type Budgets, type ProfileReport } from './gate';
import DirItem from '../components/DirItem.svelte';

// ── F20: the standalone recursive-tree fixture ──────────────────────────────────

describe('L4 fixture (M9, F20)', () => {
	it('builds a deterministic tree of ~the requested size and depth', () => {
		const a = makeTreeFixture(2000, 5);
		const b = makeTreeFixture(2000, 5);
		expect(JSON.stringify(a)).toBe(JSON.stringify(b)); // deterministic (no RNG)
		expect(countNodes(a)).toBe(2000);
		expect(treeDepth(a)).toBeLessThanOrEqual(5);
		expect(treeDepth(a)).toBeGreaterThan(1); // genuinely nested, not flat
	});

	it('the same fixture renders through the real DirItem component (drives the component, not M8 internals)', () => {
		// Proves the M9 fixture feeds an actual heavy component — F20's "drives the components via that
		// fixture". DirItem is lazy (collapsed), so we assert the top node mounts.
		const tree = makeTreeFixture(50, 3);
		const { getByText } = render(DirItem, { props: { item: tree[0], depth: 0 } });
		expect(getByText(tree[0].name)).toBeTruthy();
	});
});

// ── the profiler ────────────────────────────────────────────────────────────────

describe('L4 profiler (M9, Track L)', () => {
	it('produces a well-formed report for every heavy target', () => {
		const report = runProfile();
		for (const name of ['DirItem', 'CollectionPanel', 'ScanTreeNode']) {
			const m = report.targets[name];
			expect(m, `${name} present`).toBeTruthy();
			expect(m.nodeCount).toBe(2000);
			expect(m.scriptMs).toBeGreaterThanOrEqual(0);
			expect(m.longTaskMs).toBeGreaterThanOrEqual(0);
			expect(m.heapKb).toBeGreaterThanOrEqual(0);
		}
	});
});

// ── the gate ──────────────────────────────────────────────────────────────────

const BUDGETS: Budgets = JSON.parse(
	readFileSync(resolve(__dirname, '../../../perf-budgets.json'), 'utf8'),
);

describe('L4 budget gate (M9, Track L)', () => {
	it('the live profile passes the committed budgets (and writes the report)', () => {
		const report = runProfile();
		// Emit the JSON report (the `profiling` CI job uploads it; also handy locally).
		writeFileSync(resolve(__dirname, '../../../profiling-report.json'), JSON.stringify(report, null, 2));
		const result = checkBudgets(report, BUDGETS);
		expect(result.ok, `unexpected breaches:\n${formatBreaches(result.breaches)}`).toBe(true);
	});

	it('catches a breach, then passes again when restored (the gate actually bites)', () => {
		// Synthetic report: scriptMs deliberately over the DirItem ceiling → CI red.
		const breached: ProfileReport = {
			targets: {
				DirItem: { scriptMs: 99_999, longTaskMs: 1, heapKb: 1, nodeCount: 2000 },
				CollectionPanel: { scriptMs: 1, longTaskMs: 1, heapKb: 1, nodeCount: 2000 },
				ScanTreeNode: { scriptMs: 1, longTaskMs: 1, heapKb: 1, nodeCount: 2000 },
			},
		};
		const red = checkBudgets(breached, BUDGETS);
		expect(red.ok).toBe(false);
		expect(red.breaches.some((b) => b.target === 'DirItem' && b.metric === 'scriptMs')).toBe(true);

		// Restore the value under budget → green again (proves the gate isn't stuck-red).
		breached.targets.DirItem.scriptMs = 1;
		expect(checkBudgets(breached, BUDGETS).ok).toBe(true);
	});

	it('a changed node count (fixture/processing drift) is a deterministic breach', () => {
		const drift: ProfileReport = {
			targets: {
				DirItem: { scriptMs: 1, longTaskMs: 1, heapKb: 1, nodeCount: 1999 },
				CollectionPanel: { scriptMs: 1, longTaskMs: 1, heapKb: 1, nodeCount: 2000 },
				ScanTreeNode: { scriptMs: 1, longTaskMs: 1, heapKb: 1, nodeCount: 2000 },
			},
		};
		const res = checkBudgets(drift, BUDGETS);
		expect(res.ok).toBe(false);
		expect(res.breaches.some((b) => b.metric === 'nodeCount')).toBe(true);
	});

	it('a missing target is a breach (the profiler must cover every budgeted surface)', () => {
		const missing: ProfileReport = { targets: {} };
		expect(checkBudgets(missing, BUDGETS).ok).toBe(false);
	});
});
