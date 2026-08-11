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
		// The post-export toast copy is imported from the single source (not re-stringed in-route).
		expect(src).toContain('MANIFEST_EXPORTED_TOAST');
	});

	it('reuses the existing export path — saveDialog + exportManifest, never a new Tauri command', () => {
		// Hard constraint #3 (spec): "No new export logic; this is a second entry point to the shipped
		// one." The route must import `save as saveDialog` + `exportManifest` and call them in the click
		// handler — never a bespoke transfer/chunk command.
		const src = chatSrc();
		expect(src).toMatch(/import \{ save as saveDialog \} from '@tauri-apps\/plugin-dialog'/);
		expect(src).toContain('exportManifest');
		// The defaultPath mirrors Home → ⋯ → Export exactly (no rename, no different extension).
		const exportOpen = src.indexOf('async function handleExportManifest');
		expect(exportOpen).toBeGreaterThan(-1);
		const afterExport = src.indexOf('\tasync function', exportOpen + 1);
		const block = src.slice(exportOpen, afterExport === -1 ? src.length : afterExport);
		expect(block).toContain('defaultPath: `${slug}.hbmanifest`');
		expect(block).toContain('await exportManifest(slug, path)');
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
		// The quarantine card's onexport is an inert no-op (not wired to the handler).
		expect(quarantinedCard).toContain('onexport={() => {}}');
	});

	it('the conversation-thread card IS wired to handleExportManifest', () => {
		// The non-quarantine card (the normal conversation thread) calls the export handler. This is
		// the one place the card surfaces the verb; the handler runs the existing export path.
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
		expect(convCard).toContain('onexport={(slug) => handleExportManifest(slug)}');
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

	it('post-export copy contains no "Download"/"Send" (MAS-INV-5 + INV-4 sweeps green)', () => {
		// The toast is built from MANIFEST_EXPORTED_TOAST (single source); the route must NOT inline
		// its own copy with a forbidden word. Scan the route's user-facing segments.
		const src = chatSrc();
		const exportOpen = src.indexOf('async function handleExportManifest');
		const afterExport = src.indexOf('\tasync function', exportOpen + 1);
		const block = src.slice(exportOpen, afterExport === -1 ? src.length : afterExport);
		expect(block.toLowerCase()).not.toMatch(/\bdownload\b/);
		// "send" is allowed only inside the imported toast constant (which the helper test pins).
		// The handler body itself must not re-string a "Send" verb.
		const blockWithoutImport = block.replace(/MANIFEST_EXPORTED_TOAST/g, '');
		expect(blockWithoutImport.toLowerCase()).not.toMatch(/\bsend\b/);
	});

	it('the big-relay hint is gated on big_relay_url (read once on mount, not per-render)', () => {
		// The big-relay hint must appear only when `big_relay_url` is set. The route reads it once on
		// mount into `bigRelayUrl`, and passes `hasBigRelay={bigRelayUrl !== ''}` to both cards.
		const src = chatSrc();
		expect(src).toContain('getSettings');
		expect(src).toMatch(/bigRelayUrl = \$state\(''\)/);
		expect(src).toMatch(/bigRelayUrl = s\.big_relay_url/);
		// Both ManifestFulfilCard instances must bind hasBigRelay to bigRelayUrl (not a literal true).
		const countHasBigRelay = (src.match(/hasBigRelay=\{bigRelayUrl !== ''\}/g) || []).length;
		expect(countHasBigRelay).toBe(2);
	});

	it('drafts are loaded on mount so the card is honest without a Home visit', () => {
		// The card derives state from the owner's own drafts; the route must refresh `collections` on
		// mount so a Public-draft match doesn't silently render as "missing" because the store was empty.
		const src = chatSrc();
		expect(src).toContain('getCollections');
	});

	it('export is idempotent — a double-click while the save dialog is resolving is a no-op', () => {
		// Same idiom as W3's handleUnlock. The handler guards on `exportingSlug` (in-flight).
		const src = chatSrc();
		const exportOpen = src.indexOf('async function handleExportManifest');
		const afterExport = src.indexOf('\tasync function', exportOpen + 1);
		const block = src.slice(exportOpen, afterExport === -1 ? src.length : afterExport);
		expect(block).toMatch(/exportingSlug === slug/);
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
