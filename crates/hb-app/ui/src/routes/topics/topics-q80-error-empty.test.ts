// QURATOR-80 (H3 twin) — a fetch failure must not render as the confident negative "No public
// Topics under 'X' yet": "we could not ask" (retryable) and "the relays answered, there are none"
// are different states. QURATOR-144 W2 collapsed the six per-root error states into ONE tree-level
// `paintError`; the distinction survives on the EmptyState `error` affordance (alert role + Retry).
//
// SOURCE-SCAN (kept from the original form): the wiring claims are source-level, and jsdom computes
// no layout — a green tick does NOT prove the error branch renders as a distinct colour. Where the
// claim is behavioural (error renders on failure, Retry re-paints) the MOUNTED suite in
// topics-q85-error-cleared-on-success.test.ts owns it.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const topicsSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Topics tree — QURATOR-80 error-vs-empty distinction (W2, one tree-level state)', () => {
	it('paintError_state_exists_and_is_read_in_the_template', () => {
		// §9 W4 lesson: a symbol scan cannot prove wiring. The state must be DECLARED and READ at a
		// template branch. Both are asserted.
		const src = topicsSrc();
		expect(src).toMatch(/let\s+paintError[^=]*=\s*\$state/);
		expect(src).toMatch(/paintError\s*&&/);
	});

	it('the_error_branch_is_the_shared_EmptyState_error_affordance_with_a_retry', () => {
		// The retryable error is the EmptyState `error` variant (alert role + Retry button, the
		// QURATOR-93 surface) — not a bare muted line that would read as a confident empty.
		const src = topicsSrc();
		const idx = src.indexOf('paintError &&');
		expect(idx).toBeGreaterThan(-1);
		const branch = src.slice(idx, idx + 400);
		expect(branch).toMatch(/<EmptyState/);
		expect(branch).toMatch(/error\b/);
		expect(branch).toMatch(/onretry/);
		expect(branch).toMatch(/reach the relays/i);
	});

	it('the_catch_records_the_failure_into_paintError_and_a_retry_re_paints', () => {
		const src = topicsSrc();
		const open = src.indexOf('async function paintDirectory');
		expect(open).toBeGreaterThan(-1);
		const body = src.slice(open, src.indexOf('\n\t}', open));
		const catchIdx = body.indexOf('} catch (e) {');
		expect(catchIdx).toBeGreaterThan(-1);
		const catchSlice = body.slice(catchIdx);
		expect(catchSlice).toMatch(/paintError = true/);
		// The retry path resets both flags so paintDirectory actually re-asks. There are TWO
		// onretry sites in the file (the mine-load error's first, the paint error's second — the
		// W2 single-tree state); only the PAINT retry carries the reset, so slice from the one
		// that mentions paintError.
		const retryIdx = src.indexOf('onretry=', src.indexOf('paintError && mergedRows.length === 0'));
		expect(retryIdx).toBeGreaterThan(-1);
		const retrySlice = src.slice(retryIdx, retryIdx + 200);
		expect(retrySlice).toMatch(/painted = false/);
		expect(retrySlice).toMatch(/paintError = false/);
		expect(retrySlice).toMatch(/paintDirectory/);
	});

	it('no_confident_negative_exists_for_a_failed_paint', () => {
		// With one tree-level error state there is no per-root "No public Topics under X yet" status
		// at all anymore — the empty group simply renders zero rows. Assert the old string is gone
		// from ALL markup (comments stripped, per the §9 sentinel-collision rule — the phrase also
		// appears in a script comment explaining the contract, which is not an affordance).
		const src = topicsSrc().replace(/<!--[\s\S]*?-->/g, '').replace(/\/\/[^\n]*/g, '');
		expect(src).not.toMatch(/No public Topics under/i);
	});
});
