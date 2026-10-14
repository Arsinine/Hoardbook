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

	it('markup apostrophes never open a bogus literal (QURATOR-365)', () => {
		// Pre-fix, STRING_LITERAL_RE ran over the WHOLE comment-stripped source, markup included: the
		// apostrophe in "You're" opened a "literal" that swallowed text up to the NEXT apostrophe
		// anywhere later in the file (here yielding the bogus segment "re here. Don"), and one more
		// apostrophe anywhere later flipped the pairing (the 2026-10-13 Home regression, 0b808c9b).
		// MUTATION that reds this and the parity test below: in src/lib/copy-audit.ts, replace the
		// three literal-collection passes in extractUserFacingSegments (script blocks + {…}
		// expressions + double-quoted attributes) with a single whole-source pass
		// `while ((m = STRING_LITERAL_RE.exec(stripped))) { const val = …; if (val) segments.push(val); }`
		// — this fixture's no-bogus-segment assertion and the parity test's second expect red. Then
		// restore the file, `touch` it, and both go green again.
		const fixture = `<script>\n\tconst m = 'ok';\n</script>\n<p>You're here. Don't follow anyone.</p><span>followers</span>`;
		const hits = findForbiddenCopy(fixture);
		expect(hits).toEqual([
			{ rule: 'follow', segment: "You're here. Don't follow anyone." },
			{ rule: 'follow', segment: 'followers' },
		]);
		// The bogus standalone segment must not exist. (A substring check is vacuous here: the intact
		// text node "You're here. Don't follow anyone." itself contains "re here. Don".)
		expect(extractUserFacingSegments(fixture)).not.toContain('re here. Don');
	});

	it('apostrophe parity cannot flip the report — one more apostrophe, the same two hits', () => {
		// Same fixture with one more `'` added in a later text node. Pre-fix the pairing changed:
		// the apostrophe in "don't" then paired with the one in "It's", sweeping
		// "followers</span>" up into the bogus literal — fixture B reported a THIRD hit.
		const plain = `<script>\n\tconst m = 'ok';\n</script><p>We just follow people, don't we.</p><span>followers</span>`;
		const shifted = `<script>\n\tconst m = 'ok';\n</script><p>We just follow people, don't we.</p><span>followers</span><p>It's late.</p>`;
		const expected = [
			{ rule: 'follow', segment: "We just follow people, don't we." },
			{ rule: 'follow', segment: 'followers' },
		];
		expect(findForbiddenCopy(plain)).toEqual(expected);
		expect(findForbiddenCopy(shifted)).toEqual(expected);
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

	// QURATOR-365 follow-up (orchestrator): a markup-free .ts source is ALL code, so its literals
	// are still collected — without this the three view-models in the sweep pass vacuously.
	// MUTATION: delete the `if (scriptBlocks.length === 0 && …) { collectLiterals(stripped); return
	// segments; }` block in extractUserFacingSegments → this test reds (zero hits).
	it('a plain .ts source with no <script> and no markup still has its literals audited', () => {
		const src = "export const COPY = { a: 'follow them', b: \"shared listing\" };\n";
		expect(extractUserFacingSegments(src)).toEqual(['follow them', 'shared listing']);
		expect(findForbiddenCopy(src).map((h) => h.rule)).toEqual(['follow', 'share-wrong-sense']);
	});

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
