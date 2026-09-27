// M17 W2 → QURATOR-342 — INVERTED. This file used to pin the "Ask for access" ramp on Browse's
// `🔒 Listings locked` empty state (one button → `/chat?peer=<npub>&intent=ask-access`). Owner
// ruling 2026-09-27/28 retired it: NO Ask-for-access affordance anywhere on Browse, on an
// unreadable peer or anywhere else; an unreadable peer gets the TEASER (QURATOR-342 D2), and
// "No public collections" lost its CTA too. The file now pins the RETIREMENT, so re-introducing
// the ramp — the regression the ruling forbids — reds it. The M17-era mount-level absence pins
// live in q342-browse-teaser.test.ts and q102-browse-empty-cta.test.ts; this source-scan guard
// complements them by covering EVERY branch in one sweep (a mount test can only exercise one
// state per run — note what each cannot see, per the repo's mount-first rule, P-4).
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { extractUserFacingSegments } from '$lib/copy-audit.js';

const browseSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('QURATOR-342 — the ask ramp is retired on Browse', () => {
	it('no "Ask for access" affordance exists anywhere in the Browse page source', () => {
		// MUTATION (seen red): re-adding the M17-era locked-state button
		// (`<button ...>Ask for access</button>`) made this hit 1.
		const askButtons = browseSrc().match(/>Ask for access</g) ?? [];
		expect(askButtons.length).toBe(0);
	});

	it('no ask-access deep-link route remains (no `intent=ask-access` from this page)', () => {
		// MUTATION (seen red): restoring the `/chat?peer=…&intent=ask-access` href — with or
		// without a visible button — made this match.
		expect(browseSrc()).not.toMatch(/intent=ask-access/);
	});

	it('no new user-facing copy contains the forbidden word "Download" (MAS-INV-5)', () => {
		// The MAS-INV-5 sweep must stay green — the teaser copy must not introduce "Download".
		const offenders = extractUserFacingSegments(browseSrc())
			.map((seg) => seg.replace(/\bno[\s-]?download\b/gi, ''))
			.filter((seg) => /download/i.test(seg));
		expect(offenders).toEqual([]);
	});
});
