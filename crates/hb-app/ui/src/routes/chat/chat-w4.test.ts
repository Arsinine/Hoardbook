// M17 W4 — "Share my code" in the composer (grant leg). Source-scan guards for the chat route's
// wiring (the repo's route-page guard idiom — see chat-w3.test.ts and ask-access-w2.test.ts; the
// chat page's onMount fan-out across many api calls + $app/navigation goto makes a full mount
// heavier than the affordance check warrants). These pin the W4 hard constraints against the
// wiring, not the copy:
//   1. The composer exposes a "Share my code" control in .compose-footer (contact conversation).
//   2. The handler calls getShareCode + insertAtCursor and NEVER sendMessage/handleSend.
//   3. The tooltip carries SHARE_MY_CODE_WARNING (single source — never re-strings the copy).
//   4. The affordance lives ONLY in the one real composer (not the channel composer).
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const chatSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Chat page — M17 W4 "Share my code" composer affordance', () => {
	it('imports the pure helper (insertAtCursor) and the pinned tooltip copy', () => {
		// Structural "single source": the copy + the cursor-aware splice live in $lib/share-my-code.js,
		// never inlined in the component. The component must import both.
		const src = chatSrc();
		expect(src).toContain("from '$lib/share-my-code.js'");
		expect(src).toContain('SHARE_MY_CODE_WARNING');
		expect(src).toContain('insertAtCursor');
	});

	it('imports getShareCode (the LOCAL get_share_code — no network on the insert path)', () => {
		const src = chatSrc();
		expect(src).toMatch(/\bgetShareCode\b/);
	});

	it('exposes a "Share my code" control inside .compose-footer (the contact composer)', () => {
		// Acceptance: the button lives in the one real (contact) composer's footer, beside Send.
		// There are two .compose-footer blocks (channel + contact); the W4 button is in the contact
		// composer — the one whose Send calls handleSend. Anchor there to avoid matching the channel.
		const src = chatSrc();
		const handleSendIdx = src.indexOf('onclick={handleSend}');
		expect(handleSendIdx).toBeGreaterThan(-1);
		// Walk back to the .compose-footer that opens this Send button's footer.
		const footerOpen = src.lastIndexOf('<div class="compose-footer">', handleSendIdx);
		expect(footerOpen).toBeGreaterThan(-1);
		expect(footerOpen).toBeLessThan(handleSendIdx);
		// The share-my-code button must appear between that footer open and the Send button.
		const shareBtn = src.indexOf('share-my-code-btn', footerOpen);
		expect(shareBtn).toBeGreaterThan(-1);
		expect(shareBtn).toBeLessThan(handleSendIdx);
		// aria-label / title asserts the affordance is discoverable.
		expect(src).toContain('aria-label="Share my code"');
	});

	it('the tooltip carries SHARE_MY_CODE_WARNING (never re-strings the blunt grant line)', () => {
		// Single-source guard: the component passes SHARE_MY_CODE_WARNING to the help affordance,
		// never a literal "Anyone holding it can decrypt…".
		const src = chatSrc();
		expect(src).toMatch(/text=\{SHARE_MY_CODE_WARNING\}/);
		expect(src).not.toMatch(/Anyone holding it can decrypt your listings/);
	});

	it('the handler calls getShareCode + insertAtCursor and restores focus/caret (never sends)', () => {
		// Hard constraint: "click inserts the exact code into the draft without sending". The handler
		// must call getShareCode then insertAtCursor, then focus + setSelectionRange; it must NOT call
		// sendMessage or handleSend.
		const src = chatSrc();
		const fnOpen = src.indexOf('async function handleShareMyCode');
		expect(fnOpen).toBeGreaterThan(-1);
		const afterFn = src.indexOf('\tasync function', fnOpen + 1);
		const region = src.slice(fnOpen, afterFn === -1 ? src.length : afterFn);
		expect(region).toContain('getShareCode');
		expect(region).toContain('insertAtCursor');
		expect(region).toContain('setSelectionRange');
		expect(region).toMatch(/draftEl\?\.focus|draftEl\.focus/);
		// No-send guards: neither the network send nor the Send-button handler is invoked from here.
		expect(region).not.toContain('sendMessage');
		expect(region).not.toContain('handleSend');
	});

	it('the handler is wired to the button onclick (not a dead affordance)', () => {
		const src = chatSrc();
		expect(src).toMatch(/onclick=\{handleShareMyCode\}/);
	});

	it('the affordance is disabled only while sending (NOT on empty draft)', () => {
		// Spec: "do NOT disable on empty draft — you can share your code into an empty draft." The
		// button's disabled predicate must reference `sending`, not `draft`.
		const src = chatSrc();
		const btnOpen = src.indexOf('share-my-code-btn');
		const btnClose = src.indexOf('>', src.indexOf('<button', btnOpen - 40));
		const btnTag = src.slice(btnOpen - 60, btnClose);
		// Review follow-up: `sharingCode` (the in-flight guard) joins the predicate, but `draft`
		// still must not.
		// The predicate is exactly `sending || sharingCode` — no `draft` term.
		expect(btnTag).toMatch(/disabled=\{sending \|\| sharingCode\}/);
	});

	it('the grant is bound to the conversation it was raised in, and withdrawn on a peer switch', () => {
		// Review HIGH: the chat draft is ONE global $state, so an inserted share code otherwise
		// survives a peer switch and Send hands our browse capability to whoever is selected THEN.
		const src = chatSrc();
		const fnOpen = src.indexOf('async function handleShareMyCode');
		const region = src.slice(fnOpen, src.indexOf('\n\t}', fnOpen));
		// Captures the peer at click time and re-checks it after the await.
		expect(region).toContain('const forPeer = selectedPeer.npub');
		expect(region).toMatch(/selectedPeer\?\.npub !== forPeer/);
		expect(region).toContain('sharedCodeInDraft = { npub: forPeer, code }');
		// selectPeer withdraws a grant bound to a different peer.
		const sel = src.indexOf('async function selectPeer');
		const selRegion = src.slice(sel, src.indexOf('\n\t}', sel));
		expect(selRegion).toContain('withdrawInsert(draft, sharedCodeInDraft.code)');
		expect(selRegion).toContain('sharedCodeInDraft.npub !== peer.npub');
	});

	it('the share-code fetch is single-flight and yields to an in-flight send', () => {
		// Review MEDIUM: a double-click otherwise splices two codes (the second against a stale
		// caret), and a Send that lands mid-fetch otherwise gets its emptied draft repopulated.
		const src = chatSrc();
		const fnOpen = src.indexOf('async function handleShareMyCode');
		const region = src.slice(fnOpen, src.indexOf('\n\t}', fnOpen));
		expect(region).toMatch(/if \(sending \|\| sharingCode \|\| !selectedPeer\) return/);
		expect(region).toContain('sharingCode = true');
		expect(region).toContain('sharingCode = false');
		expect(region).toMatch(/\|\| sending\) return/);
	});

	it('the affordance lives ONLY in the contact composer, not the channel composer', () => {
		// The grant affordance is for contact conversations (the grant-a-stranger case happens after
		// Q7-accept, which is a contact conversation). The channel composer's footer must NOT carry it.
		const src = chatSrc();
		// Find the channel composer footer (the first .compose-footer, used by sendChannelPost).
		const channelFooter = src.indexOf('<div class="compose-footer">');
		expect(channelFooter).toBeGreaterThan(-1);
		const channelSendIdx = src.indexOf('sendChannelPost', channelFooter);
		expect(channelSendIdx).toBeGreaterThan(-1);
		const channelFooterEnd = src.indexOf('</div>', channelSendIdx);
		const channelRegion = src.slice(channelFooter, channelFooterEnd);
		expect(channelRegion).not.toContain('share-my-code-btn');
	});
});
