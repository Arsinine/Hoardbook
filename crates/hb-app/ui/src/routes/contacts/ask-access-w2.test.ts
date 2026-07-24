// M17 W2 — "Ask for access" ramps on every locked surface in Contacts. Source-scan guard following
// the repo's route-page idiom (contacts-w1.test.ts): the contacts page's heavy onMount and
// `$app/navigation` goto make a full mount heavier than the affordance wiring check warrants.
//
// The locked contact card's `.access-hint` line gains exactly one "Ask for access" button that
// routes to `/chat?peer=<npub>&intent=ask-access` with the petname carried via `&petname=` so the
// draft reads naturally. The W1 discovery `messagePeer` callback now uses the compose deep-link
// with the same intent (discovery hits are keyless by design → always start with the ask prefill).
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { extractUserFacingSegments } from '$lib/copy-audit.js';

const contactsSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Contacts page — M17 W2 ask-access ramps', () => {
	it('locked contact card exposes exactly one Ask-for-access affordance', () => {
		// The `.access-hint` div (rendered only when badge.locked) gains a single "Ask for access"
		// button → `/chat?peer=<npub>&intent=ask-access`. Count affordances: exactly one per card.
		const src = contactsSrc();
		const askButtons = src.match(/>Ask for access</g) ?? [];
		expect(askButtons.length).toBe(1);
	});

	it('ask-access button routes to the chat peer deep-link with the ask-access intent', () => {
		const src = contactsSrc();
		// The button sits inside the access-hint block; its onclick builds the deep-link URL with
		// the intent param and the petname.
		expect(src).toMatch(/intent=ask-access/);
		expect(src).toMatch(/peer=.*npub/);
	});

	it('ask-access button carries the petname via the petname query param', () => {
		// The petname is passed in the URL so askAccessDraft reads naturally. encodeURIComponent is
		// used so a petname with spaces/special chars survives the round trip.
		const src = contactsSrc();
		expect(src).toMatch(/petname=/);
		expect(src).toMatch(/encodeURIComponent/);
	});

	it('the W1 discovery messagePeer callback now uses the compose deep-link with ask-access intent', () => {
		// Ramp (c): discovery hits are keyless by design, so the W1 Message callback routes to
		// `/chat?compose=<npub>&intent=ask-access` (always — no petname, the stranger case).
		const src = contactsSrc();
		// The messagePeer function (W1) now includes the intent param on its compose deep-link.
		const fnOpen = src.indexOf('function messagePeer');
		expect(fnOpen).toBeGreaterThan(-1);
		const fnClose = src.indexOf('}', fnOpen);
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
