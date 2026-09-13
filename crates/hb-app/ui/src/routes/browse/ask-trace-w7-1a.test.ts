// M17 W7.1a -> QURATOR-203 — the manual manifest-ask round trip was deleted from the paywall
// (fetch/serve are fully automatic: fetch_driver.rs, auto_approve.rs). Still pinned here:
//   - "MAS-INV-5" — none of the paywall copy contains "Download".
//   - QURATOR-203 — the round-trip buttons stay out of the markup (scoped to button text, P-12).
//   - api.ts still carries the persisted ask-trace getter — the Rust command keeps its production
//     callers (the fetch driver); only the UI wiring died.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { extractUserFacingSegments } from '$lib/copy-audit.js';

const browseSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Browse page — M17 W7.1a the ask leaves a trace', () => {
	it('no new user-facing copy contains the forbidden word "Download" (MAS-INV-5)', () => {
		// The invariant sweep must stay green — none of the W7.1a copy introduces "Download".
		const offenders = extractUserFacingSegments(browseSrc())
			.map((seg) => seg.replace(/\bno[\s-]?download\b/gi, ''))
			.filter((seg) => /download/i.test(seg));
		expect(offenders).toEqual([]);
	});

	it('QURATOR-203: the paywall no longer renders the manifest-ask/import round-trip buttons', () => {
		// Fetch/serve are now fully automatic (fetch_driver.rs, auto_approve.rs) — the "Ask the
		// owner", "Import a manifest file", "or paste it", "Ask a contact" and "Ask them" buttons
		// are dead chrome and were deleted from the paywall markup. Scoped to button text/role, not
		// page prose (P-12), since the panel's own copy can legitimately contain these words.
		const src = browseSrc();
		expect(src).not.toMatch(/>Ask the owner for the full list</);
		expect(src).not.toMatch(/>Import a manifest file you received</);
		expect(src).not.toMatch(/>or paste it</);
		expect(src).not.toMatch(/>Ask a contact for this list</);
		expect(src).not.toMatch(/>Ask them</);
	});

	it('the ask is recorded inside request_manifest (server-side, after send_dm_inner) — api.ts carries the getter', () => {
		// Structural guard: the getter is wired so the route can read the persisted map back. The
		// actual "record after success" invariant is pinned in the Rust store tests; this asserts the
		// frontend has a way to read it.
		const apiSrc = readFileSync(new URL('../../lib/api.ts', import.meta.url), 'utf8');
		expect(apiSrc).toContain('getManifestAsks');
		expect(apiSrc).toContain('get_manifest_asks');
		expect(apiSrc).toContain('interface ManifestAsk');
	});
});
