// QURATOR-80 (H3 twin) — Discover must not render a fetch failure as the confident negative
// "No public Topics under 'X' yet." That string is indistinguishable from a genuine empty, a
// timeout, an empty relay set, or an untagged announce. The surface must distinguish "we could not
// ask" (retryable) from "the relays answered, there are none".
//
// This is a SOURCE-SCAN test (the repo's route-page pattern; see contacts-w1.test.ts and the W9
// shell guard in this folder). The topics page's onMount fan-out + `$app/navigation` makes a full
// mount heavier than the affordance check warrants. Per CLAUDE.md §9:
//   - jsdom computes no layout — a green tick does NOT prove the page *renders* the error branch as
//     a distinct colour. It proves only that the source *says* the right things. The visual
//     distinction is asserted as source text (`root-error` class + `--error` token), not as a
//     render, and that limitation is stated here, not implied.
//   - Strip comments before asserting absence: the error branch's own comment documents the
//     confident-negative string ("NOT the confident negative 'No public Topics under X yet'"), so a
//     raw whole-text scan for that string reds on the file's own documentation — the sentinel
//     collision CLAUDE.md §9 warns about. We assert the error branch's TEMPLATE TEXT, not its
//     comment, and we assert the branch ORDER (error before genuine-empty) on the template slice.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const topicsSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

/** The expanded-root template region — from the `{#if expandedRoot === root}` open to its close.
 *  This is the slice where the loading / error / empty / list branch lives. */
function expandedRegion(src: string): string {
	const open = src.indexOf('{#if expandedRoot === root}');
	expect(open).toBeGreaterThan(-1);
	// The matching close: the `{/if}` that ends the expandedRoot conditional. It is the first
	// `{/if}` at the same depth after the open. We walk forward counting nested `{#if`.
	let depth = 0;
	const ifRe = /\{#if\s|\{:else if\s|\{:else\s|\{\/if\}/g;
	let m: RegExpExecArray | null;
	let closeIdx = -1;
	while ((m = ifRe.exec(src.slice(open))) !== null) {
		const token = m[0];
		if (token.startsWith('{#if')) depth++;
		else if (token.startsWith('{/if}')) {
			depth--;
			if (depth === 0) { closeIdx = open + m.index + token.length; break; }
		}
	}
	expect(closeIdx).toBeGreaterThan(open);
	return src.slice(open, closeIdx);
}

describe('Topics Discover — QURATOR-80 error-vs-empty distinction', () => {
	it('erroredRoots_state_exists_and_is_read_in_the_template', () => {
		// §9 W4 lesson: a symbol scan over a whole file cannot prove an identifier is wired. The
		// state must be DECLARED and READ at a template branch (the error affordance lives in the
		// template, not the script). Both are asserted.
		const src = topicsSrc();
		expect(src).toMatch(/let\s+erroredRoots[^=]*=\s*\$state/);
		expect(src).toMatch(/erroredRoots\.has\(root\)/);
	});

	it('the_catch_branch_records_a_failure_into_erroredRoots_not_a_cached_empty', () => {
		// The defect's other half: a failed fetch must NOT be cached as `rootTopics[root] = []`,
		// or the retry reads the cached empty and never re-asks. The catch must write the error
		// set. Pin the write site (not just the symbol) — the §9 call-site rule.
		const src = topicsSrc();
		// There are several catch blocks; find the one inside toggleRoot.
		const toggleOpen = src.indexOf('async function toggleRoot');
		expect(toggleOpen).toBeGreaterThan(-1);
		const toggleClose = src.indexOf('\t}', src.indexOf('finally {', toggleOpen));
		const toggleBody = src.slice(toggleOpen, toggleClose);
		const catchInToggle = toggleBody.indexOf("} catch (e) {");
		expect(catchInToggle).toBeGreaterThan(-1);
		const catchSlice = toggleBody.slice(catchInToggle);
		expect(catchSlice).toMatch(/erroredRoots/);
		expect(catchSlice).toMatch(/next\.add\(root\)/);
		// On retry (re-expand of an errored root), the error is cleared so a fresh fetch runs —
		// an error is never replayed as if it were a cached answer.
		expect(toggleBody).toMatch(/next\.delete\(root\)/);
	});

	it('error_branch_text_is_distinct_from_the_confident_negative', () => {
		// The genuine-empty status reads "No public Topics under X yet." The error status must say
		// something clearly different (a reachability / retry message), so an unknown is never read
		// as a confident empty. Assert the error branch carries reachability language AND the retry
		// affordance — both on the error slice, not the whole file.
		const src = topicsSrc();
		const region = expandedRegion(src);
		const errorBranch = region.slice(
			region.indexOf('erroredRoots.has(root)'),
			region.indexOf('length === 0')
		);
		expect(errorBranch.length).toBeGreaterThan(0);
		expect(errorBranch).toMatch(/reach the relays|Couldn’t reach/i);
		expect(errorBranch).toMatch(/re-expand|retry/i);
	});

	it('error_branch_carries_a_distinct_root_error_class_on_the_error_token_ramp', () => {
		// The genuine-empty status is `class="root-status muted"` (the muted ramp). The error status
		// must carry a distinct class (`root-error`) styled on the shared `--error` token, so the two
		// read visually different — not as the same muted line. jsdom cannot prove it RENDERS
		// differently (no layout); this asserts the source WIRING (class + token), and the visual
		// claim is stated, not implied.
		const src = topicsSrc();
		const region = expandedRegion(src);
		const errorBranch = region.slice(
			region.indexOf('erroredRoots.has(root)'),
			region.indexOf('length === 0')
		);
		expect(errorBranch).toMatch(/class="root-status root-error"/);
		// The style declaration exists and is on the --error token (same ramp as .btn-danger).
		expect(src).toMatch(/\.root-error\s*\{\s*color:\s*var\(--error\)/);
		// The genuine-empty line stays on the muted ramp (unchanged — regression guard against the
		// twin drifting the OTHER way).
		expect(src).toMatch(/No public Topics under/);
	});

	it('error_branch_precedes_the_genuine_empty_branch_in_the_template', () => {
		// The order matters: the error check must come BEFORE the empty-length check, or a failed
		// fetch whose cache was left undefined falls through to the confident negative. Pin the
		// order on the template slice.
		const src = topicsSrc();
		const region = expandedRegion(src);
		const errorIdx = region.indexOf('erroredRoots.has(root)');
		const emptyIdx = region.indexOf('length === 0');
		expect(errorIdx).toBeGreaterThan(-1);
		expect(emptyIdx).toBeGreaterThan(-1);
		expect(errorIdx).toBeLessThan(emptyIdx);
	});

	it('the_confident_negative_string_does_not_appear_in_the_error_branch_template', () => {
		// §9 sentinel-collision guard: strip comments first. The error branch's COMMENT documents the
		// very string we're asserting absence of ("NOT the confident negative 'No public Topics under
		// X yet'"), so a raw scan of the whole branch reds on its own documentation. Assert against
		// TEMPLATE MARKUP only (strip HTML comments + the prose in comments).
		const src = topicsSrc();
		const region = expandedRegion(src);
		const errorBranch = region.slice(
			region.indexOf('erroredRoots.has(root)'),
			region.indexOf('length === 0')
		);
		// Strip HTML comments — the documentation, not the affordance.
		const noComments = errorBranch.replace(/<!--[\s\S]*?-->/g, '');
		expect(noComments).not.toMatch(/No public Topics under/i);
	});
});

