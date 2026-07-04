import { describe, expect, it } from 'vitest';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { extractUserFacingSegments, findForbiddenCopy } from './copy-audit.js';

describe('copy-audit engine — Q4/Q6: "Contact" replaces follow*, "share" reserved for the share code', () => {
	it('"Copy share code" is clean — the allowlisted sense of share', () => {
		expect(findForbiddenCopy('<button>Copy share code</button>')).toEqual([]);
	});

	it('"follow them" is a violation', () => {
		const hits = findForbiddenCopy(`<p>Look up a peer above and follow them.</p>`);
		expect(hits).toEqual([{ rule: 'follow', segment: 'Look up a peer above and follow them.' }]);
	});

	it('identifiers and on:click expressions are not extracted', () => {
		const src = `<script>\n\tfunction handleFollow() {}\n\tlet followedNpubs = new Set();\n</script>\n<button on:click={handleFollow}>Add contact</button>`;
		expect(findForbiddenCopy(src)).toEqual([]);
	});

	it('<style> blocks are stripped — a CSS selector like .hit-following never trips the audit', () => {
		const src = `<style>\n.hit-following { color: red; }\n</style>\n<p>Copy share code</p>`;
		expect(findForbiddenCopy(src)).toEqual([]);
	});

	it('"shared listing" is a violation (the wrong, publish-flavored sense of share)', () => {
		const hits = findForbiddenCopy(`<p>Re-publish a shared listing when its folder changes.</p>`);
		expect(hits.some((h) => h.rule === 'share-wrong-sense')).toBe(true);
	});

	it('"SMB share" is clean — an explicitly allowed sense', () => {
		expect(findForbiddenCopy(`<p>Reconcile poll for collections edited over an SMB share.</p>`)).toEqual([]);
	});

	it('// and /* */ comments are stripped, and import lines are ignored', () => {
		const src = [
			"// TODO: stop following people",
			"/* legacy: unfollow flow removed */",
			"import { follow } from './api.js';",
			'<p>Add contact</p>',
		].join('\n');
		expect(findForbiddenCopy(src)).toEqual([]);
	});

	it('extractUserFacingSegments collects quoted literals and markup text, trimmed and non-empty', () => {
		const src = `<script>\n\tconst msg = 'Added';\n</script>\n<p>Hello {name}, welcome</p>`;
		const segments = extractUserFacingSegments(src);
		expect(segments).toContain('Added');
		expect(segments.some((s) => s.includes('Hello') && s.includes('welcome'))).toBe(true);
		expect(segments.every((s) => s.trim().length > 0)).toBe(true);
	});
});

// ── Repo-wide sweep — MUST be green once the Q4/Q6 copy migration is complete. ─────────────────────
// Scans every .svelte under routes/ + lib/components/, plus tooltips.ts, topics-view.ts, and
// private-collections-view.ts (the copy-bearing pure view-models). Excludes api.ts, types.ts, and any
// *.test.ts (test fixtures/strings are not user-facing copy).
describe('copy-audit — repo-wide sweep (Q4/Q6 migration)', () => {
	const here = path.dirname(fileURLToPath(import.meta.url));
	const uiSrc = path.resolve(here, '.'); // .../ui/src/lib
	const srcRoot = path.resolve(uiSrc, '..'); // .../ui/src

	function walkSvelte(dir: string): string[] {
		const out: string[] = [];
		for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
			const full = path.join(dir, entry.name);
			if (entry.isDirectory()) out.push(...walkSvelte(full));
			else if (entry.isFile() && entry.name.endsWith('.svelte')) out.push(full);
		}
		return out;
	}

	function scanSet(): string[] {
		const files: string[] = [];
		files.push(...walkSvelte(path.join(srcRoot, 'routes')));
		files.push(...walkSvelte(path.join(srcRoot, 'lib', 'components')));
		files.push(
			path.join(srcRoot, 'lib', 'tooltips.ts'),
			path.join(srcRoot, 'lib', 'topics-view.ts'),
			path.join(srcRoot, 'lib', 'private-collections-view.ts'),
		);
		// Belt-and-braces: never scan a test file or the explicitly excluded modules.
		return files.filter(
			(f) => !f.endsWith('.test.ts') && !f.endsWith(`${path.sep}api.ts`) && !f.endsWith(`${path.sep}types.ts`),
		);
	}

	const files = scanSet();

	it('scans a non-trivial set of files', () => {
		expect(files.length).toBeGreaterThan(10);
	});

	it('every scanned file has zero forbidden-copy hits', () => {
		const failures: string[] = [];
		for (const file of files) {
			const source = fs.readFileSync(file, 'utf-8');
			const hits = findForbiddenCopy(source);
			if (hits.length > 0) {
				failures.push(`${path.relative(srcRoot, file)}:\n${hits.map((h) => `  [${h.rule}] "${h.segment}"`).join('\n')}`);
			}
		}
		expect(failures, failures.join('\n\n')).toEqual([]);
	});
});
