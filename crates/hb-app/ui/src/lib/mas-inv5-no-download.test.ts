import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { extractUserFacingSegments } from './copy-audit.js';
import {
	SEND_FULL_LIST_LABEL,
	SEND_FULL_LIST_TOAST,
	SEND_FULL_LIST_FALLBACK,
	REDEEMING_LINE,
	REDEEMED_LINE,
	REDEEM_FAILED_LINE,
	REDEEM_RETRY_LABEL,
} from './transport-ticket.js';

// MAS-INV-5 (Hoardbook stays neutral in the transfer): the paywall / browse surface must offer NO
// "Download" affordance — Hoardbook moves no collection files (INV-4′). The "get the rest" path is "Ask the owner
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

// M18 W4 — the fulfil verb added a THIRD route to a full listing: the owner clicks "Send the full
// list" and a manifest crosses a QUIC connection. That is exactly the shape MAS-INV-5 was written to
// watch, so the guard extends to cover it.
//
// **The negative above is unchanged and is not narrowed.** No allowlist entry was added, no scanned
// file was dropped, and the browse-surface scan still forbids "download" outright. What follows is
// additional coverage of a surface that did not exist when that scan was written — the two chat cards
// through which a listing can now be requested, approved, and received.
//
// The line MAS-INV-5 draws is between a LISTING and a FILE. M18 authorizes the first and still forbids
// the second (INV-4′), so the copy on these surfaces must never offer to fetch, download, or send a
// user's files — only the list of them.
describe('MAS-INV-5 — the M18 fulfil surfaces move listings, never files', () => {
	const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), 'utf8');

	const FULFIL_SURFACES = [
		'./components/ManifestFulfilCard.svelte',
		'./components/TransportTicketCard.svelte',
	];

	// ⚠ **Scanning the components alone would have been a near-vacuous guard, and it was.** These two
	// cards keep their copy in exported constants (one source, shared with the chat route and these
	// tests), so `extractUserFacingSegments` finds barely any literal text in the markup —
	// TransportTicketCard yields exactly ONE user-facing string. A scan of the components would have
	// passed no matter what the cards actually say.
	//
	// Caught by printing what the scan extracted instead of trusting that it extracted something. Same
	// lesson as the INV-4′ sweep that went silently green on a renamed file: **run the guard and look
	// at what it saw.**
	//
	// So the copy is scanned AT ITS SOURCE. `COPY` renders every user-facing string these surfaces can
	// show, and `copy-source-completeness` below fails if a new one is exported without being added
	// here — which is what stops this guard from quietly going vacuous again.
	const COPY: string[] = [
		SEND_FULL_LIST_LABEL,
		SEND_FULL_LIST_TOAST('criterion'),
		SEND_FULL_LIST_FALLBACK,
		REDEEMING_LINE('criterion'),
		REDEEMED_LINE('criterion'),
		REDEEM_FAILED_LINE,
		REDEEM_RETRY_LABEL,
	];

	/** Every exported copy constant must be covered by `COPY`. Without this, adding a
	 *  `SEND_THE_FILES_LABEL` tomorrow would sail past every scan below — the guard would still be
	 *  green and still be checking the same seven strings. */
	it('copy-source-completeness — every exported copy constant is scanned', async () => {
		const mod = await import('./transport-ticket.js');
		// Copy is identified by the module's naming convention: UPPER_SNAKE_CASE is copy, camelCase is
		// machinery (parsers, the ledger). `TICKET_TAG` is the one UPPER_SNAKE export that is a wire
		// discriminator rather than a string a user reads.
		//
		// **Known limit, stated rather than papered over:** copy exported under a camelCase name would
		// evade this. The convention holds across the module today and the failure message names the
		// offending value, so a violation is one grep from being understood.
		const rendered = Object.entries(mod)
			.filter(([name]) => /^[A-Z][A-Z0-9_]*$/.test(name) && name !== 'TICKET_TAG')
			.map(([, v]) =>
				typeof v === 'function' ? String((v as (s: string) => string)('criterion')) : String(v),
			);
		expect(rendered.length).toBeGreaterThan(0); // the scan must actually scan something
		for (const r of rendered) {
			expect(COPY, `un-scanned copy constant with value: ${r}`).toContain(r);
		}
	});

	it('no user-facing "Download" copy on any fulfil surface', () => {
		const segments = [
			...COPY,
			...FULFIL_SURFACES.flatMap((rel) => extractUserFacingSegments(read(rel))),
		];
		const offenders = segments
			.map((seg) => seg.replace(ALLOWED_NEGATIVE, ''))
			.filter((seg) => /download/i.test(seg));
		expect(offenders).toEqual([]);
	});

	/** The negative word-scan alone would pass on copy that offered files without ever saying
	 *  "download" — "Send them the files", "Get the files". The invariant is about the OBJECT of the
	 *  verb, so scan for that object directly.
	 *
	 *  **Scoped to a CLAUSE, and that scoping is implemented — not merely asserted in this comment.**
	 *  The reassurance MAS-INV-5 *wants* present ("…your files stay where they are") sits in the same
	 *  sentence as an offer verb governing something else ("They get the listing"), so a
	 *  whole-sentence scan flags it. It did, on the first run. The honest fix is to split on clause
	 *  boundaries so each verb is tested against its own object — an offer verb and "files" in ONE
	 *  clause is an offer; in two clauses it is a contrast. A hand-written allowlist for that one
	 *  string would have hidden the next real offence that happened to resemble it. */
	const clauses = (s: string) => s.split(/[.!?;,—–\n]+/);
	it('nothing on a fulfil surface offers to move the files themselves', () => {
		const segments = [
			...COPY,
			...FULFIL_SURFACES.flatMap((rel) => extractUserFacingSegments(read(rel))),
		];
		const offenders = segments
			.flatMap(clauses)
			.filter((c) => /\b(send|get|fetch|receive|transfer|download|share)\b/i.test(c))
			.filter((c) => /\bfiles?\b/i.test(c));
		expect(offenders).toEqual([]);
	});

	/** The other half of the same claim: the surfaces must positively say what DOES cross. Copy that
	 *  merely avoided the word "files" while describing nothing would pass every scan above. */
	it('the fulfil copy names the LIST as the thing that crosses', () => {
		expect(SEND_FULL_LIST_LABEL).toMatch(/\blist\b/i);
		expect(SEND_FULL_LIST_TOAST('criterion')).toMatch(/\blist(ing)?\b/i);
		expect(REDEEMED_LINE('criterion')).toMatch(/\blist\b/i);
		// …and says out loud that the files do not move.
		expect(SEND_FULL_LIST_TOAST('criterion')).toMatch(/files stay/i);
	});

	/** Positive assertion, mirroring the browse one: the ratified M18 affordances are present, so a
	 *  regression that removed the verb — or replaced it with something that moves files — is caught
	 *  rather than silently passing a negative scan. */
	it('the fulfil card offers "Send the full list" AND keeps export reachable', () => {
		const src = read('./components/ManifestFulfilCard.svelte');
		expect(src).toContain('SEND_FULL_LIST_LABEL');
		// Export is the fallback for when the transport cannot connect. If it ever disappears, the
		// owner is left with one route that can fail and no second one — the dead end W7.1b removed.
		expect(src).toContain('Export manifest…');
	});

	/** The asker's card fires its redemption on render. It must therefore never grow a button that
	 *  offers a *deferred* fetch: the backend has no deferred entry point (owner ruling 2026-07-30),
	 *  so such a button could only be a lie or a second, divergent code path. Retry after a failure is
	 *  the one permitted button, and the ledger bounds it to the failed state. */
	it('the ticket card offers no deferred "fetch/download now" affordance', () => {
		const offenders = extractUserFacingSegments(read('./components/TransportTicketCard.svelte')).filter(
			(seg) => /\b(fetch|get|download|redeem)\s+(it\s+)?(now|later)\b/i.test(seg),
		);
		expect(offenders).toEqual([]);
	});
});
