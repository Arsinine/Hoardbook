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
		// `onexport` is gone entirely (owner ruling 2026-08-27) — the no-op that used to stand in for
		// "inert here" no longer exists, so what this pins now is that the quarantine card is handed
		// an inert `onsend` and nothing else actionable.
		expect(quarantinedCard).not.toContain('onexport');
		expect(quarantinedCard).toContain('onsend={() => {}}');
	});

	// INVERTED with the button (owner ruling 2026-08-27). Was: "the conversation-thread card IS
	// wired to handleExportManifest". The conversation card now has exactly one action — Send — so
	// what needs pinning is that the send wiring survived the removal intact.
	it('the conversation-thread card is wired to send, and to nothing else', () => {
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
		expect(convCard).toContain('onsend={(slug) => handleSendFullList(slug, mf.request.askNonce)}');
		expect(convCard).not.toContain('onexport');
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

	// QURATOR-45: the "Send the full list" click guard must not permanently swallow retries after a
	// failed or hung first attempt. The guard blocks CONCURRENT double-clicks (so two approvals are
	// never minted for one request), but the `finally` block MUST clear `sendingFullList` on BOTH
	// success and failure — otherwise a bind that hangs or fails leaves the button permanently
	// disabled and every subsequent click silently discarded. Source-scan guards (route-page idiom):
	//   1. The guard sets sendingFullList = slug (blocking concurrent clicks).
	//   2. The finally block resets sendingFullList = null (releasing the guard for retries).
	//   3. The catch block surfaces the error (so a failed send is visible, not silent).
	it('the send-full-list guard releases on failure so retries are not permanently swallowed', () => {
		const src = chatSrc();
		const sendOpen = src.indexOf('async function handleSendFullList');
		expect(sendOpen).toBeGreaterThan(-1);
		const afterSend = src.indexOf('\tasync function', sendOpen + 1);
		const block = src.slice(sendOpen, afterSend === -1 ? src.length : afterSend);
		// Guard blocks concurrent clicks (prevents double-approval).
		expect(block).toMatch(/sendingFullList === slug/);
		expect(block).toMatch(/sendingFullList = slug/);
		// The finally block MUST clear the guard — without this, a hung/failed first attempt
		// permanently swallows every subsequent click (the QURATOR-45 defect).
		expect(block).toMatch(/finally\s*\{[\s\S]*sendingFullList = null/);
		// The catch block surfaces the error as a toast (not silent) — a hung bind that returns an
		// Err now reaches this catch and is visible.
		expect(block).toMatch(/catch\s*\(e\)\s*\{[\s\S]*toast\(String\(e\),\s*['"]error['"]\)/);
	});

	it('the send-full-list handler has a catch AND finally (both are required for recovery)', () => {
		// If the catch or finally were removed, a failed send would either swallow the error
		// (no catch) or leave the guard stuck (no finally). Both must be present.
		const src = chatSrc();
		const sendOpen = src.indexOf('async function handleSendFullList');
		const afterSend = src.indexOf('\tasync function', sendOpen + 1);
		const block = src.slice(sendOpen, afterSend === -1 ? src.length : afterSend);
		expect(block).toMatch(/catch\s*\(e\)/);
		expect(block).toMatch(/finally\s*\{/);
	});
});
