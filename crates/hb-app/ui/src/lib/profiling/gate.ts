// M9 Track L — the L4 budget gate. Pure: compares a profile report to the checked-in budgets and
// reports any metric that breached its ceiling (× the tolerance). A breach fails the `profiling` CI
// job. Seeded generously; tighten on a clean run (spec §Testing → L4; the baseline is launch
// calibration, like the count freshness window).

import type { ProfileReport, TargetMetrics } from './profile';

/** A per-target ceiling for each budgeted metric. Omit a metric to leave it unbudgeted. */
export interface TargetBudget {
	scriptMs?: number;
	longTaskMs?: number;
	heapKb?: number;
	/** Exact expected node count (deterministic) — a mismatch means the fixture/processing changed. */
	nodeCount?: number;
}

export interface Budgets {
	/** Multiplicative slack applied to the ms/heap ceilings (e.g. 1.0 = strict, 1.5 = +50%). */
	tolerance: number;
	targets: Record<string, TargetBudget>;
}

export interface Breach {
	target: string;
	metric: string;
	value: number;
	budget: number;
}

export interface GateResult {
	ok: boolean;
	breaches: Breach[];
}

/** Compare a report against budgets; return every metric that exceeded its (toleranced) ceiling. */
export function checkBudgets(report: ProfileReport, budgets: Budgets): GateResult {
	const tol = budgets.tolerance > 0 ? budgets.tolerance : 1;
	const breaches: Breach[] = [];

	for (const [target, budget] of Object.entries(budgets.targets)) {
		const metrics = report.targets[target];
		if (!metrics) {
			breaches.push({ target, metric: 'present', value: 0, budget: 1 });
			continue;
		}
		// nodeCount is deterministic → exact match, no tolerance.
		if (budget.nodeCount !== undefined && metrics.nodeCount !== budget.nodeCount) {
			breaches.push({ target, metric: 'nodeCount', value: metrics.nodeCount, budget: budget.nodeCount });
		}
		checkCeiling(breaches, target, 'scriptMs', metrics, budget, tol);
		checkCeiling(breaches, target, 'longTaskMs', metrics, budget, tol);
		checkCeiling(breaches, target, 'heapKb', metrics, budget, tol);
	}

	return { ok: breaches.length === 0, breaches };
}

function checkCeiling(
	breaches: Breach[],
	target: string,
	metric: 'scriptMs' | 'longTaskMs' | 'heapKb',
	metrics: TargetMetrics,
	budget: TargetBudget,
	tol: number,
): void {
	const ceiling = budget[metric];
	if (ceiling === undefined) return;
	const limit = ceiling * tol;
	if (metrics[metric] > limit) {
		breaches.push({ target, metric, value: metrics[metric], budget: limit });
	}
}

/** Human-readable one-liner per breach (for the CI log). */
export function formatBreaches(breaches: Breach[]): string {
	return breaches
		.map((b) => `  ${b.target}.${b.metric} = ${b.value} > budget ${b.budget}`)
		.join('\n');
}
