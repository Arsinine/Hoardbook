// M17 W3 — the received-share-code card (consume leg). Source-scan guards for the chat route's
// wiring (the repo's established pattern for route-page guards — see contacts-w1.test.ts and
// mas-inv5-no-download.test.ts; the chat page's onMount fan-out across many api calls + $app/navigation
// goto makes a full mount heavier than the affordance check warrants). These pin the W3 hard
// constraints against the wiring, not the copy:
//   1. Zero-network on render — detection invokes only LOCAL Tauri commands (validate_share_code +
//      share_code_info); paste_key/follow fire ONLY inside the click handler.
//   2. Quarantine renders inert — inside the Q7 request inbox the card renders but with NO action
//      buttons (Accept comes first, always).
//   3. The card is an addendum — the verbatim message text always renders above it (never replaced).
//   4. One card per message — a multi-code message renders the first valid candidate only.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const chatSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Chat page — M17 W3 received-share-code card wiring', () => {
	it('imports the ShareCodeCard component + the zero-network detection helpers', () => {
		// The card is a distinct component (avatar-less compact block); the detection helper is pure.
		const src = chatSrc();
		expect(src).toContain("from '$lib/components/ShareCodeCard.svelte'");
		expect(src).toContain("from '$lib/share-code-detect.js'");
		expect(src).toContain('extractShareCodeCandidate');
	});

	it('detection invokes only LOCAL Tauri commands (validate_share_code + share_code_info)', () => {
		// Hard constraint #1: zero network on render. The detection path (ensureDetected) must call
		// validateShareCode + shareCodeInfo (both local) and NEVER pasteKey/follow (both network).
		// Source-scan the ensureDetected function body (from its declaration to the next top-level
		// `async function`/`function ` boundary).
		const src = chatSrc();
		const ensureOpen = src.indexOf('async function ensureDetected');
		expect(ensureOpen).toBeGreaterThan(-1);
		// The next function declaration after ensureDetected is detectedFor (a plain function).
		const ensureClose = src.indexOf('\n\tfunction detectedFor', ensureOpen);
		expect(ensureClose).toBeGreaterThan(ensureOpen);
		const ensureBlock = src.slice(ensureOpen, ensureClose);
		expect(ensureBlock).toContain('validateShareCode');
		expect(ensureBlock).toContain('shareCodeInfo');
		// pasteKey + follow must NOT appear inside detection — they live only in the click handler.
		expect(ensureBlock).not.toContain('pasteKey');
		expect(ensureBlock).not.toContain('await follow(');
	});

	it('paste_key/follow fire ONLY inside the click handler (resolution on explicit user action)', () => {
		// Hard constraint #5: a human decides. The network surface (pasteKey/follow) is behind the
		// Unlock click, never on render. They appear in handleUnlock (the click handler), NOT in
		// ensureDetected (the render path).
		const src = chatSrc();
		const unlockOpen = src.indexOf('async function handleUnlock');
		expect(unlockOpen).toBeGreaterThan(-1);
		// Find the function body end (next 'async function' or a top-level closing brace).
		const afterUnlock = src.indexOf('\tasync function', unlockOpen + 1);
		const unlockBlock = src.slice(unlockOpen, afterUnlock === -1 ? src.length : afterUnlock);
		expect(unlockBlock).toContain('pasteKey');
		expect(unlockBlock).toContain('await follow(');
	});

	it('quarantine card renders with NO action buttons (Accept comes first)', () => {
		// Hard constraint #3: inside the Q7 request inbox, a detected code renders as the card
		// visually but with quarantined={true} and zero action handlers. The Accept/Decline/Block
		// row below the thread is the only surface until the stranger is accepted.
		const src = chatSrc();
		const reqBlock = src.indexOf('{#each req.messages as msg}');
		expect(reqBlock).toBeGreaterThan(-1);
		// Find the ShareCodeCard inside the request bucket.
		const cardAfterReq = src.indexOf('<ShareCodeCard', reqBlock);
		expect(cardAfterReq).toBeGreaterThan(-1);
		const cardEnd = src.indexOf('/>', cardAfterReq);
		const quarantinedCard = src.slice(cardAfterReq, cardEnd);
		expect(quarantinedCard).toContain('quarantined={true}');
		// The quarantine card's onunlock/onaddcontact are inert no-ops (not wired to handlers).
		expect(quarantinedCard).toContain('onunlock={() => {}}');
		expect(quarantinedCard).toContain('onaddcontact={() => {}}');
	});

	it('the card is an addendum — verbatim message text renders above it, never replaced', () => {
		// Hard constraint #4: text-only chat stays text-only; the card is an ADDENDUM below the
		// verbatim `pre-wrap` text. The bubble-text <p> must precede the ShareCodeCard in the markup.
		const src = chatSrc();
		const convBlock = src.indexOf('{#each conversation as msg');
		expect(convBlock).toBeGreaterThan(-1);
		const bubbleText = src.indexOf('class="bubble-text"', convBlock);
		const shareCard = src.indexOf('ShareCodeCard', convBlock);
		expect(bubbleText).toBeGreaterThan(-1);
		expect(shareCard).toBeGreaterThan(-1);
		expect(bubbleText).toBeLessThan(shareCard);
	});

	it('caches detection per message id so re-renders are free', () => {
		// The cache is structural: `detectedCodes` is keyed by message id and checked before any
		// Tauri call. A chat history full of codes costs zero relay round-trips on re-render.
		const src = chatSrc();
		expect(src).toMatch(/detectedCodes.*\[\s*messageId\s*\]|messageId in detectedCodes/);
	});

	it('unlock is idempotent (a double-click while resolving is a no-op)', () => {
		// Acceptance criterion: double-click Unlock is idempotent. The handler guards on
		// `unlockingCode` (in-flight) + `unlockedCodes` (already done).
		const src = chatSrc();
		const unlockOpen = src.indexOf('async function handleUnlock');
		const afterUnlock = src.indexOf('\tasync function', unlockOpen + 1);
		const unlockBlock = src.slice(unlockOpen, afterUnlock === -1 ? src.length : afterUnlock);
		expect(unlockBlock).toMatch(/unlockingCode === code/);
		expect(unlockBlock).toMatch(/unlockedCodes\.has\(code\)/);
	});
});
