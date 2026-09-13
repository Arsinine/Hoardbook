// M17 W2 — "Ask for access" ramp on Browse's `🔒 Listings locked` empty state. Source-scan guard
// following the repo's route-page idiom (mas-inv5-no-download.test.ts, contacts-w1.test.ts).
//
// The locked-listings empty state gains exactly one "Ask for access" button that routes to
// `/chat?peer=<npub>&intent=ask-access`. Browse's selectedPeer is a CachedPeer (has petname), so
// the petname is carried via `&petname=` for a natural draft.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { extractUserFacingSegments } from '$lib/copy-audit.js';

const browseSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Browse page — M17 W2 ask-access ramp on Listings locked', () => {
	it('Listings locked empty state exposes exactly one Ask-for-access affordance', () => {
		const src = browseSrc();
		const askButtons = src.match(/>Ask for access</g) ?? [];
		expect(askButtons.length).toBe(1);
	});

	it('ask-access button sits inside the Listings locked empty state', () => {
		// The button lives in the `listingsLocked` branch. The paywall block's manifest-ask
		// affordances (QURATOR-203: round-trip buttons removed) no longer exist to bound against.
		const src = browseSrc();
		const lockIdx = src.indexOf('🔒 Listings locked');
		expect(lockIdx).toBeGreaterThan(-1);
		const askIdx = src.indexOf('>Ask for access<');
		expect(askIdx).toBeGreaterThan(lockIdx);
	});

	it('ask-access button routes to the chat peer deep-link with the ask-access intent', () => {
		const src = browseSrc();
		expect(src).toMatch(/intent=ask-access/);
		expect(src).toMatch(/petname=/);
	});

	it('no new user-facing copy contains the forbidden word "Download" (MAS-INV-5)', () => {
		// The MAS-INV-5 sweep must stay green — the ask-access copy must not introduce "Download".
		const offenders = extractUserFacingSegments(browseSrc())
			.map((seg) => seg.replace(/\bno[\s-]?download\b/gi, ''))
			.filter((seg) => /download/i.test(seg));
		expect(offenders).toEqual([]);
	});
});
