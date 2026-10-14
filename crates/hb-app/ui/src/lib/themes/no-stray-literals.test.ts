import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

/**
 * Lane A (QURATOR-352): no stray colour literals inside svelte <style> blocks.
 *
 * Every route/component style block must take colours from tokens. Literals
 * that legitimately remain are explicit per-file allowlist entries below.
 *
 * P-10 mutation (seen red, see lane report): reintroduce
 * `color: oklch(0.5 0.1 60);` into a swapped line (CollectionRow's .size-warn)
 * — this test must go red; restoring var(--warn) goes green.
 */

// .../ui/src — vitest root is the ui/ dir (see vite.config.ts test.include)
const SRC = join(process.cwd(), 'src');
const ROOTS = ['routes', 'lib/components'];

/**
 * Literals left in place, keyed by path relative to src/ (posix separators).
 * Each needle is the EXACT substring allowed to remain; occurrences are removed
 * from the scanned text, so a line carrying both an allowed literal and a stray
 * one still fails.
 *
 * OUT OF FENCE: 'routes/+layout.svelte' is not a Lane-A file; its shadow-pop
 * literal is left for the orchestrator to swap to var(--shadow-pop) — remove
 * that allowlist entry when the swap lands.
 */
const ALLOWED: Record<string, string[]> = {
	// NOT A LANE-A FILE — mid-flight sibling/orchestrator additions; near-miss values
	// with no exact token. (Its old shadow-pop literal was tokenised to
	// var(--shadow-pop) during this pass — marked by the QURATOR-352 comment there.)
	'routes/+layout.svelte': [
		'oklch(0.25 0.06 25)',
		'oklch(0.65 0.18 25 / 0.4)',
		'oklch(0.85 0.12 25)',
	],
	// shadow/border near-misses with no exact token
	'routes/+page.svelte': [
		'1px solid oklch(0.65 0.18 25 / 0.35)',
		'0 8px 28px -6px oklch(0.78 0.14 70 / 0.18)',
		'0 1px 2px rgba(0, 0, 0, 0.25)',
		'0 8px 24px oklch(0 0 0 / 0.25)',
	],
	// avatar/directory hue gradient — not a theme colour
	'routes/browse/+page.svelte': ['oklch(0.55 0.15 var(--dg-hue))'],
	'routes/contacts/+page.svelte': [
		'oklch(0.55 0.15 var(--dg-hue))',
		'oklch(0.78 0.15 75)',
		'0 8px 24px oklch(0 0 0 / 0.3)',
		'oklch(0.75 0.12 60)',
		'oklch(0.82 0.14 60)',
	],
	'routes/chat/+page.svelte': [
		'oklch(0.22 0.06 60 / 0.6)',
		'1px solid oklch(0.45 0.12 60 / 0.3)',
	],
	// NOT SWAPPED BY LANE A — arrived mid-flight from a sibling lane editing settings;
	// amber-family near-misses with no exact token. Orchestrator to rule on tokenising.
	'routes/settings/+page.svelte': [
		'oklch(0.78 0.15 75)',
		'oklch(0.5 0.15 75 / 0.4)',
		'oklch(0.28 0.08 75 / 0.5)',
	],
	// warm hover family, near-miss values with no exact token
	'lib/components/CollectionRow.svelte': [
		'oklch(0.75 0.14 60)',
		'oklch(0.25 0.06 60)',
		'1px solid oklch(0.45 0.10 60 / 0.4)',
	],
	'lib/components/ShareCodeCard.svelte': [
		'inset 0 0 0 1px oklch(1 0 0 / 0.12)',
		'oklch(0.78 0.14 35)',
	],
	'lib/components/PersonRow.svelte': ['inset 0 0 0 1px oklch(1 0 0 / 0.12)'],
	// hue gradients + one-off shadows/tints with no exact token
	'lib/components/AddContactPanel.svelte': [
		'linear-gradient(135deg, oklch(0.30 0.10 280) 0%, oklch(0.25 0.12 320) 100%)',
	],
	// close-button hover on red — deliberate literal (order: leave)
	'lib/components/WindowControls.svelte': ['#fff'],
	// shadow-pop near-miss (alpha 0.3, not 0.4) — order: leave
	'lib/components/OverflowMenu.svelte': ['0 8px 24px oklch(0 0 0 / 0.3)'],
};

function walk(dir: string, out: string[] = []): string[] {
	for (const e of readdirSync(dir, { withFileTypes: true })) {
		const p = join(dir, e.name);
		if (e.isDirectory()) walk(p, out);
		else if (e.name.endsWith('.svelte')) out.push(p);
	}
	return out;
}

describe('no stray colour literals in svelte <style> blocks', () => {
	const files = ROOTS.flatMap((r) => walk(join(SRC, r))).sort();
	const keyOf = (abs: string) => abs.slice(SRC.length + 1).split('\\').join('/');

	it('examines a sane number of files', () => {
		console.log(`examined ${files.length} svelte files under routes + lib/components`);
		expect(files.length).toBeGreaterThan(20);
		for (const must of [
			'routes/+page.svelte',
			'routes/browse/+page.svelte',
			'routes/contacts/+page.svelte',
			'routes/chat/+page.svelte',
			'routes/settings/+page.svelte',
			'routes/+layout.svelte',
			'lib/components/Modal.svelte',
			'lib/components/Avatar.svelte',
			'lib/components/CollectionRow.svelte',
		]) {
			expect(
				files.some((f) => keyOf(f) === must),
				`coverage: ${must} was NOT examined (0-files-examined guard)`
			).toBe(true);
		}
	});

	it('finds no literal outside the allowlist; every allowlist needle is used', () => {
		const offenders: string[] = [];
		const perFile: string[] = [];
		const used = new Set<string>();
		for (const f of files) {
			const key = keyOf(f);
			const text = readFileSync(f, 'utf8');
			const styles = [...text.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)];
			const allow = ALLOWED[key] ?? [];
			let hits = 0;
			for (const m of styles) {
				// strip CSS comments BEFORE allowlisting and scanning (order requirement)
				let block = m[1].replace(/\/\*[\s\S]*?\*\//g, '');
				allow.forEach((needle, i) => {
					if (block.includes(needle)) {
						used.add(`${key}|${i}`);
						block = block.split(needle).join('\u0000');
					}
				});
				for (const line of block.split('\n')) {
					if (/oklch\(|rgba?\(|#[0-9a-fA-F]{3,8}\b/.test(line)) {
						hits++;
						offenders.push(`${key}: ${line.trim()}`);
					}
				}
			}
			perFile.push(`${key}: ${hits} hits (${styles.length} style block(s))`);
		}
		console.log(perFile.join('\n'));
		// every allowlist entry must actually occur in its file — catches stale
		// entries and needle typos (an unused needle would silently widen the gate)
		for (const [key, needles] of Object.entries(ALLOWED)) {
			for (let i = 0; i < needles.length; i++) {
				expect(
					used.has(`${key}|${i}`),
					`stale allowlist entry: "${needles[i]}" never occurs in ${key} (typo? already swapped?)`
				).toBe(true);
			}
		}
		expect(
			offenders,
			`stray colour literals (${offenders.length}):\n${offenders.join('\n')}`
		).toEqual([]);
	});
});
