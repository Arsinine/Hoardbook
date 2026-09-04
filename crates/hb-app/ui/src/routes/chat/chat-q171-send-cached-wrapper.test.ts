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
// Mutation probe (must RED the surviving test):
//   - add `const { invoke } = await import('@tauri-apps/api/core')` anywhere in the chat page's
//     script block → the no-raw-invoke test reds.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const chatSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');
const apiSrc = () => readFileSync(new URL('../../lib/api.ts', import.meta.url), 'utf8');

describe('QURATOR-171 — send_cached_manifest goes through api.ts, not a raw dynamic import', () => {
	/* ⚠ Two assertions here were DELETED 2026-09-04 (QURATOR-164), not weakened: they pinned that
	 * `api.ts` exports `sendCachedManifest` and that the chat page imports it. The owner deleted the
	 * fulfil verb — public collections need no approval, so nothing offers a click — and the
	 * `send_cached_manifest` Tauri command went with it, since a registered command with zero callers
	 * that mints a ticket and DMs it is attack surface rather than dead code. There is no wrapper left
	 * to pin.
	 *
	 * The third assertion below is KEPT and is the one that still earns its place: it guards the
	 * *general* rule the defect violated — the chat page must never reach Tauri directly, bypassing
	 * api.ts's `isTauri` guard — and that applies to every command, not just the deleted one. */

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
