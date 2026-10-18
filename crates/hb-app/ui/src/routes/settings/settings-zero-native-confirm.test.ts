// @vitest-environment jsdom
// QURATOR-379 — the owner's bug: Settings used the plugin's NATIVE OS confirm() popups for
// destructive consents. All three sites (plaintext-export, plaintext-restore warning, restore
// replace-all) now go through the page's own in-app Modal via askInApp(). This file pins the
// regression fence:
//
//   1. The plugin-dialog import STATEMENT in ./+page.svelte names `open` and `save` but NOT
//      `confirm` (P-7: slice the statement, never scan the whole file — a call site alone proves
//      nothing about what is imported). Fail-closed: re-adding the import reds.
//   2. No `confirm(` call form survives anywhere in the page's comment-stripped source (P-12:
//      anchor on syntax, not the bare identifier — prose mentioning confirm does not count).
//
// MUTATIONS THAT MUST RED (both seen red, restored by re-applying the inverse edit):
//   M-A  add `, confirm` to the plugin-dialog import statement in ./+page.svelte
//        → test 1 reds on the import assertion.
//   M-B  replace `askInApp('Restore from backup', …)`'s body usage in doRestore: make askInApp
//        resolve true WITHOUT setting confirmState (no modal ever opens)
//        → settings-q126-validate-before-wipe.test.ts reds (its modalButton() waitFor times out);
//          the decline test in settings-q275 reds the same way. That file's suites are the
//          driving-half proof; this file is the import-half proof.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

// (process.cwd()-relative, as contacts-m22-drag does — a `new URL('./+page.svelte',
// import.meta.url)` gets rewritten by Vite's asset-URL handling and is no longer file-scheme.)
const PAGE = resolve(process.cwd(), 'src/routes/settings/+page.svelte');

describe('Settings never raises a NATIVE confirm dialog (QURATOR-379 in-app Modal)', () => {
	const src = readFileSync(PAGE, 'utf8');	// Strip comments before asserting (P-12) so a prose mention can never satisfy the scan.
	const stripped = src.replace(/<!--[\s\S]*?-->/g, '').replace(/\/\*[\s\S]*?\*\//g, '').replace(/^[ \t]*\/\/.*$/gm, '');

	it('the plugin-dialog import statement names open and save but NOT confirm', () => {
		// P-7: slice the import STATEMENT and assert against it — a whole-file scan is satisfied
		// by any stray mention.
		const stmt = stripped.match(/import\s*\{([^}]*)\}\s*from\s*'@tauri-apps\/plugin-dialog'/);
		expect(stmt, 'the plugin-dialog import statement must exist').toBeTruthy();
		// Each clause is `NAME` or `NAME as alias` — take the SOURCE name (before any `as`).
		const sourceNames = stmt![1]
			.split(',')
			.map((s) => s.trim())
			.filter(Boolean)
			.map((c) => c.split(/\s+as\s+/)[0]!.trim());
		expect(sourceNames).toContain('open');
		expect(sourceNames).toContain('save');
		expect(sourceNames).not.toContain('confirm');
	});

	it('no native confirm( call form exists in the page source', () => {
		// The call FORM, not the bare identifier (P-12): prose and the askInApp( calls are fine.
		expect(stripped).not.toMatch(/\bconfirm\s*\(/);
	});
});
