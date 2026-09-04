// QURATOR-164 build item 4 — the pure predicate for the one-time BASELINE startup notice.
// Same shape as onboarding.test.ts's `shouldShowPrivacyNotice` block: direct unit tests, no
// mounting, no jsdom (this file is the cheap half; the runtime wiring is covered by the fact
// that +layout.svelte imports showServingNoticeOnce — see the report for the layout-mount pin).
import { describe, it, expect } from 'vitest';
import { shouldShowServingNotice } from './serving-notice.js';

describe('QURATOR-164 — baseline serving notice predicate', () => {
	it('serving_notice_shown_once: shown iff not yet acknowledged', () => {
		// MUTATION (P-10): in serving-notice.ts, in the `shouldShowServingNotice` function body,
		// change `return !settings.serving_notice_acknowledged;` to
		// `return settings.serving_notice_acknowledged;` (drop the negation) — this test reds on
		// both lines: the unacknowledged case reads false and the acknowledged case reads true.
		expect(shouldShowServingNotice({ serving_notice_acknowledged: false })).toBe(true);
		expect(shouldShowServingNotice({ serving_notice_acknowledged: true })).toBe(false);
	});

	it('a settings object straight off a pre-QURATOR-164 disk file (no such key) is treated as unacknowledged', () => {
		// The Rust `#[serde(default)]` half loads a missing key as false; this pins the TS half
		// of the same contract. A stale object without the key would arrive as `undefined`, and
		// `!undefined` is true — so the predicate still shows the notice. MUTATION (P-10): in
		// serving-notice.ts, in the `shouldShowServingNotice` function body, change
		// `return !settings.serving_notice_acknowledged;` to `return false;` — this line reds.
		const legacy = {} as { serving_notice_acknowledged: boolean };
		expect(shouldShowServingNotice(legacy)).toBe(true);
	});
});
