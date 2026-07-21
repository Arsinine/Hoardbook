import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { extractUserFacingSegments } from './copy-audit.js';

// MAS-INV-5 (Hoardbook stays neutral in the transfer): the paywall / browse surface must offer NO
// "Download" affordance — Hoardbook moves no files (INV-4). The "get the rest" path is "Ask the owner
// for the full list" (a DM) + "Import a manifest file you received" (a local file consume), never a
// download button. This guard scans the browse page's USER-FACING copy (copy-audit strips class=,
// comments, imports, <style>) and forbids the word "download" — except the allowed *negative* sense,
// the note that Hoardbook does NOT download (the `no-download` tooltip key / footer). The allowlist is
// word-boundary-anchored so it strips only a real "no download" / "no-download" — never the tail of
// another word (so "Casino download" is still caught).
const ALLOWED_NEGATIVE = /\bno[\s-]?download\b/gi;

describe('MAS-INV-5 — no Download affordance in the browse/paywall surface', () => {
	const browseSrc = () =>
		readFileSync(new URL('../routes/browse/+page.svelte', import.meta.url), 'utf8');

	it('the browse page shows no user-facing "Download" copy', () => {
		const offenders = extractUserFacingSegments(browseSrc())
			.map((seg) => seg.replace(ALLOWED_NEGATIVE, ''))
			.filter((seg) => /download/i.test(seg));
		expect(offenders).toEqual([]);
	});

	it('the paywall offers the "ask by DM" + "import" affordances (never a download)', () => {
		// Positive assertion: the ratified "get the rest" affordances are present, so a regression that
		// removed them (or swapped in a Download) is caught, not just the negative word-scan.
		const src = browseSrc();
		expect(src).toContain('Ask the owner for the full list');
		expect(src).toContain('Import a manifest file you received');
	});
});
