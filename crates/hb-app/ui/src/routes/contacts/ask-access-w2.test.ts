// M17 W2 — "Ask for access" on the locked contact card, UPDATED 2026-09-28 for QURATOR-342 lane B:
// the button is REMOVED by owner ruling ("No Ask-for-access button on an unreadable peer" — an
// unreadable person shows the reason, the teaser, in Browse), so the pins that demanded exactly one
// button, its chat deep-link and its petname param are replaced by a pin on its ABSENCE. The W1
// discovery `messagePeer` callback keeps the compose deep-link with the ask-access intent (discovery
// hits are keyless by design → always start with the ask prefill) and is pinned unchanged.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { extractUserFacingSegments } from '$lib/copy-audit.js';

const contactsSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Contacts page — M17 W2 ask-access ramps', () => {
	it('the locked contact card offers NO Ask-for-access affordance (QURATOR-342 removal)', () => {
		// Was: exactly one "Ask for access" button per locked card. Owner ruling 2026-09-27/28
		// (QURATOR-342 COMMON): no ask button; an unreadable person just shows the reason in Browse.
		// Count affordances: ZERO, and no ask-access-btn class anywhere in the page.
		const src = contactsSrc();
		const askButtons = src.match(/>Ask for access</g) ?? [];
		expect(askButtons.length).toBe(0);
		expect(src).not.toMatch(/ask-access-btn/);
	});

	it('the discovery Message action (now in DiscoverBook) keeps the compose deep-link with ask-access intent for a stranger', () => {
		// Ramp (c): discovery hits are keyless by design, so a stranger's Message routes to
		// `/chat?compose=<npub>&intent=ask-access`. QURATOR-360/361: the W1 hit-cards and the page's
		// messagePeer are gone; the one discovery surface is DiscoverBook, so the pin moved there.
		// MUTATION (seen red 2026-10-14): in DiscoverBook.svelte replace the `'&intent=ask-access'`
		// branch of `const intent = …` with `''` → reds.
		const src = readFileSync(new URL('../../lib/components/DiscoverBook.svelte', import.meta.url), 'utf8');
		const fnOpen = src.indexOf('function message(');
		expect(fnOpen).toBeGreaterThan(-1);
		const fnClose = src.indexOf('\n\t}', fnOpen);
		const fnBody = src.slice(fnOpen, fnClose);
		expect(fnBody).toMatch(/compose=/);
		expect(fnBody).toMatch(/intent=ask-access/);
	});

	it('no new user-facing copy contains the forbidden word "Download" (MAS-INV-5)', () => {
		// INV sweep: the ask-access copy must not introduce "Download". Uses the same copy-audit
		// extractor as mas-inv5-no-download.test.ts so we see only what a user reads.
		const offenders = extractUserFacingSegments(contactsSrc()).filter((seg) =>
			/download/i.test(seg),
		);
		expect(offenders).toEqual([]);
	});
});
