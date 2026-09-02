// QURATOR-171 — Defect B: the chat page's `handleServeCached` did
// `const { invoke } = await import('@tauri-apps/api/core')` and invoked `send_cached_manifest`
// directly — a fan-out artifact whose own comment named the intended cleanup (an api.ts wrapper).
// That bypass also skipped api.ts's `isTauri` guard, so a browser/dev-context click would reject
// with the raw Tauri error instead of the file's uniform one.
//
// Source-scan guards here are the REPO'S established idiom for wiring pins (chat-w3/w4/w7), but
// per the import-scan rule (CLAUDE.md §9 / P-7) a symbol scan over a whole file cannot prove an
// import exists — the call site satisfies it. So the import assertions slice out the import
// STATEMENT itself and assert against that; the no-raw-invoke assertion strips comments first so
// it can never red on its own explanation.
//
// Mutation probes (each must RED its named test):
//   - restore the raw `await import('@tauri-apps/api/core')` in handleServeCached → both tests red;
//   - delete `sendCachedManifest` from api.ts → the api.ts tests red.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const chatSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');
const apiSrc = () => readFileSync(new URL('../../lib/api.ts', import.meta.url), 'utf8');

/** Slice ONE import statement out of a source string: from the `import {` opening on the line that
 *  starts the module's import block through the closing `} from '…';`. Anchors on the imported
 *  symbol so the slice is the statement that actually binds it (P-7 — the call site satisfies a
 *  bare symbol scan, so the assertion must run against the statement). */
function importStatementFor(src: string, symbol: string): string | null {
	const re = new RegExp(`import\\s*\\{[^}]*\\b${symbol}\\b[^}]*\\}\\s*from\\s*'[^']+';`, 'm');
	const m = src.match(re);
	return m ? m[0] : null;
}

describe('QURATOR-171 — send_cached_manifest goes through api.ts, not a raw dynamic import', () => {
	it("api.ts exports sendCachedManifest invoking the command (the wrapper the Lane C note named)", () => {
		const src = apiSrc();
		expect(src).toContain("export const sendCachedManifest");
		expect(src).toContain("'send_cached_manifest'");
	});

	it("the chat page's import statement binds sendCachedManifest from $lib/api.js (sliced, not symbol-scanned)", () => {
		const stmt = importStatementFor(chatSrc(), 'sendCachedManifest');
		// Loud precondition: a null here means the import line is GONE, which is exactly the defect
		// this guards — never a pass.
		expect(stmt).not.toBeNull();
		expect(stmt).toContain("from '$lib/api.js'");
	});

	it('the chat page carries NO raw @tauri-apps import or invoke — every Tauri call lives in api.ts', () => {
		// Strip comments first (P-12): the deletion left prose naming the old path; an absence
		// assertion must red on the affordance, not on its own explanation.
		const src = chatSrc()
			.replace(/<!--[\s\S]*?-->/g, '')
			.replace(/\/\*[\s\S]*?\*\//g, '')
			.replace(/^\s*\/\/.*$/gm, '');
		expect(src).not.toContain("@tauri-apps/api/core");
		expect(src).not.toMatch(/\binvoke\s*\(/);
		// The command name appears ONLY in prose-free code: with comments stripped there is no
		// legitimate reason for the page to name the snake_case command at all.
		expect(src).not.toContain('send_cached_manifest');
	});
});
