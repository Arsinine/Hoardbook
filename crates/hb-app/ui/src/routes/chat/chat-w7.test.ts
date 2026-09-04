// M17 W7.1b — the manifest-request fulfilment card wiring on the chat route. Source-scan guards
// (the repo's established pattern for route-page guards — see chat-w3.test.ts and
// mas-inv5-no-download.test.ts; the chat page's onMount fan-out across many api calls + $app/navigation
// goto makes a full mount heavier than the affordance check warrants). These pin the W7.1b hard
// constraints against the wiring, not the copy:
//   1. The request bubble becomes an actionable card (addendum, never a replacement).
//   2. State is derived PURELY (zero network on render) — `manifestFulfilFor` wraps parse + derive.
//   3. The Public case invokes the exact existing export path (saveDialog → exportManifest → toast).
//   4. Quarantine (Q7 request inbox) renders ZERO action buttons.
//   5. Post-export copy contains no "Download"/"Send" (MAS-INV-5 + INV-4 sweeps green).
//   6. The big-relay hint appears only when `big_relay_url` is set.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const chatSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Chat page — M17 W7.1b manifest-request fulfilment card wiring', () => {
	it('imports ManifestFulfilCard + the pure helper (single source for state + copy)', () => {
		const src = chatSrc();
		expect(src).toContain("from '$lib/components/ManifestFulfilCard.svelte'");
		expect(src).toContain("from '$lib/manifest-fulfil.js'");
		expect(src).toContain('manifestFulfilFor');
	});

	// INVERTED by the owner's 2026-08-27 ruling: "Remove the export manifest button - the entire
	// purpose of this IS so you dont have to manually find a third party service to send your files".
	// Chat had a whole second export entry point (saveDialog → exportManifest → toast) behind that
	// button; button and path are both gone. This test used to pin the path's shape; it now pins its
	// ABSENCE, so a future edit cannot quietly reintroduce a second export surface in Chat.
	it('Chat carries NO export path at all — export lives on Home, not here', () => {
		const src = chatSrc();
		expect(src).not.toContain('handleExportManifest');
		expect(src).not.toContain('saveDialog');
		expect(src).not.toContain('exportManifest');
		expect(src).not.toContain('MANIFEST_EXPORTED_TOAST');
		expect(src).not.toContain('exportingSlug');
		// The card itself must not grow one either. Comments stripped first: the deletion left a note
		// naming the button and where Export still lives, and an absence assertion must not red on
		// its own explanation (CLAUDE.md §9 — the affordance, not the word).
		const card = readFileSync(
			new URL('../../lib/components/ManifestFulfilCard.svelte', import.meta.url),
			'utf8',
		)
			.replace(/<!--[\s\S]*?-->/g, '')
			.replace(/\/\*[\s\S]*?\*\//g, '')
			.replace(/^\s*\/\/.*$/gm, '');
		expect(card).not.toMatch(/Export manifest/);
		expect(card).not.toContain('onexport');
	});

	it('the Private state offers no big-relay advice (it cannot work for a sealed collection)', () => {
		// Review fix: a Private collection is sealed per recipient, so "add a big relay and publish"
		// would not get THIS asker anything — it is advice that cannot work. The Private branch is
		// the inert explanatory line and nothing else; the big-relay hint belongs to the exportable
		// (Public) case, which is the only one where publishing actually closes the loop.
		const card = readFileSync(
			new URL('../../lib/components/ManifestFulfilCard.svelte', import.meta.url),
			'utf8',
		);
		const priv = card.indexOf("state.kind === 'private'");
		const nextBranch = card.indexOf("state.kind === 'empty'", priv);
		expect(priv).toBeGreaterThan(-1);
		expect(nextBranch).toBeGreaterThan(priv);
		const branch = card.slice(priv, nextBranch);
		expect(branch).toContain('MANIFEST_PRIVATE_LINE');
		expect(branch).not.toContain('MANIFEST_BIG_RELAY_LINK');
		expect(branch).not.toContain('MANIFEST_BIG_RELAY_HINT');
	});

	it('quarantine (Q7 request inbox) renders the card with ZERO action buttons', () => {
		// Hard constraint #5: inside the Q7 request inbox the fulfilment card renders visually (so the
		// owner can see what the request is about) but with the state derived under
		// `{ quarantined: true }` (which forces the inert `quarantine` branch — Accept first, always)
		// and an inert onexport no-op. The Accept/Decline/Block row is the only surface until the
		// stranger is accepted.
		const src = chatSrc();
		const reqEach = src.indexOf('{#each req.messages as msg}');
		expect(reqEach).toBeGreaterThan(-1);
		const cardAfterReq = src.indexOf('<ManifestFulfilCard', reqEach);
		expect(cardAfterReq).toBeGreaterThan(-1);
		const cardEnd = src.indexOf('/>', cardAfterReq);
		const quarantinedCard = src.slice(cardAfterReq, cardEnd);
		// The derivation call above the card must pass `{ quarantined: true }`.
		const reqBlockStart = src.lastIndexOf('{#if manifestFulfilFor', cardAfterReq);
		const reqBlock = src.slice(reqBlockStart, cardAfterReq);
		expect(reqBlock).toContain('quarantined: true');
		// QURATOR-164: the card now takes NO action props at all — the send verb was deleted with the
		// approval, so "inert here" is no longer a special case of the quarantine branch, it is the
		// card's only mode. The `quarantined: true` derivation above still matters (it forces the
		// Accept-first copy), which is why this test survives rather than merging with the one below.
		for (const prop of ['onexport', 'onsend', 'onserve', 'sending']) {
			expect(quarantinedCard).not.toContain(prop);
		}
	});

	// INVERTED TWICE. 2026-08-27 (owner) turned "wired to handleExportManifest" into "wired to
	// Send". 2026-09-04 (owner, QURATOR-164) removed Send as well: public collections need no
	// approval, so the conversation card is wired to NOTHING and the auto-approve loop answers the
	// request. What still needs pinning is the `quarantined: false` derivation — that is the only
	// thing distinguishing this call site from the quarantine one above.
	it('the conversation-thread card is wired to no action at all', () => {
		const src = chatSrc();
		const convEach = src.indexOf('{#each conversation as msg');
		expect(convEach).toBeGreaterThan(-1);
		const cardAfterConv = src.indexOf('<ManifestFulfilCard', convEach);
		expect(cardAfterConv).toBeGreaterThan(-1);
		const cardEnd = src.indexOf('/>', cardAfterConv);
		const convCard = src.slice(cardAfterConv, cardEnd);
		// The derivation call above the card must pass `{ quarantined: false }`.
		const convBlockStart = src.lastIndexOf('{#if manifestFulfilFor', cardAfterConv);
		const convBlock = src.slice(convBlockStart, cardAfterConv);
		expect(convBlock).toContain('quarantined: false');
		for (const prop of ['onexport', 'onsend', 'onserve', 'sending']) {
			expect(convCard).not.toContain(prop);
		}
	});

	it('the card is an ADDENDUM — verbatim message text renders above it, never replaced', () => {
		// Hard constraint (spec): "the request bubble BECOMES an actionable card" but the same rule as
		// W3 applies — the verbatim text always renders. The bubble-text <p> precedes the card.
		const src = chatSrc();
		const convEach = src.indexOf('{#each conversation as msg');
		expect(convEach).toBeGreaterThan(-1);
		const bubbleText = src.indexOf('class="bubble-text"', convEach);
		const mfCard = src.indexOf('ManifestFulfilCard', convEach);
		expect(bubbleText).toBeGreaterThan(-1);
		expect(mfCard).toBeGreaterThan(-1);
		expect(bubbleText).toBeLessThan(mfCard);
	});

	// REMOVED WITH THE HINT (owner ruling 2026-08-27): the card's big-relay line ("Add a big relay in
	// Settings to publish the rest for them.") was one of the three lines the owner cut, so
	// `hasBigRelay` is no longer a prop and there is no gate left to test. What remains worth pinning
	// is that no card resurrects it.
	it('no ManifestFulfilCard takes a big-relay prop any more', () => {
		const src = chatSrc();
		expect(src).not.toContain('hasBigRelay');
		const card = readFileSync(
			new URL('../../lib/components/ManifestFulfilCard.svelte', import.meta.url),
			'utf8',
		);
		expect(card).not.toContain('hasBigRelay');
		expect(card).not.toContain('MANIFEST_BIG_RELAY_LINK');
		expect(card).not.toContain('MANIFEST_BIG_RELAY_HINT');
	});

	it('drafts are loaded on mount so the card is honest without a Home visit', () => {
		// The card derives state from the owner's own drafts; the route must refresh `collections` on
		// mount so a Public-draft match doesn't silently render as "missing" because the store was empty.
		const src = chatSrc();
		expect(src).toContain('getCollections');
	});

	/* ⚠ TWO TESTS DELETED HERE 2026-09-04 (QURATOR-164), and the reason is not "they were noisy".
	 * They pinned `handleSendFullList`'s concurrency guard — that a `finally` released
	 * `sendingFullList` so a failed attempt did not permanently swallow every later click, and that
	 * a `catch` surfaced the error rather than dropping it. That was the QURATOR-45 defect, and the
	 * guard was worth having.
	 *
	 * The handler itself is gone: the owner deleted the send verb because public collections need no
	 * approval, so there is no click to guard and no in-flight state to release. The equivalent
	 * concern now lives in Rust, in the auto-approve loop, which logs a failed serve and continues
	 * rather than wedging — and a failure there leaves the request in the hold queue rather than
	 * lost. Do not restore these tests without first restoring a handler for them to guard. */
});
