// M17 W2 — the chat deep-link `?peer=<npub>&intent=ask-access` (and the `?compose=` variant) must
// populate the composer draft via the pure `applyAskAccessIntent` helper, send NOTHING, and never
// clobber an existing draft. The no-send / no-clobber LOGIC is unit-tested in ask-access.test.ts
// (against the pure helper); this file pins that the chat page actually wires that helper to the
// URL params without reaching `sendMessage`. It follows the repo's route-page guard idiom
// (contacts-w1.test.ts, mas-inv5-no-download.test.ts) because the chat page's onMount fan-out
// across many api calls and `$app/navigation`'s `goto` make a full mount heavier than the wiring
// check warrants.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const chatSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Chat page — M17 W2 ask-access deep-link', () => {
	it('reads both intent=ask-access and petname query params', () => {
		// The deep-link effect reads `intent` and `petname` from the URL and feeds them to the pure
		// helper. The `peer` param is already read by the existing effect; this asserts the W2
		// additions are present.
		const src = chatSrc();
		expect(src).toMatch(/searchParams\.get\(['"]intent['"]\)/);
		expect(src).toMatch(/searchParams\.get\(['"]petname['"]\)/);
	});

	it('routes the intent through applyAskAccessIntent (never re-strings the copy)', () => {
		// The copy lives in ONE place ($lib/ask-access.ts); the component must call the helper, not
		// inline "Hi — could I have your share code…". This is the structural "single source" guard.
		const src = chatSrc();
		expect(src).toMatch(/applyAskAccessIntent/);
		expect(src).not.toMatch(/could I have your share code/);
	});

	it('the intent path populates draft (or composeBody), not sendMessage', () => {
		// "No auto-send": the intent only writes composer-draft text locally. Assert the intent
		// handling touches `draft` / `composeBody` and does NOT call sendMessage. (sendMessage stays
		// the Send-button path; the intent path must not reach it.)
		const src = chatSrc();
		// The intent handling writes to the composer state variables — either inline
		// (`draft = applyAskAccessIntent(...).draft`) or via the applied-result form
		// (`const applied = applyAskAccessIntent(...); draft = applied.draft`, which also
		// carries the .focus flag).
		expect(src).toMatch(/(?:draft|composeBody)\s*=\s*applied\.draft|draft\s*=\s*[^;]*applyAskAccessIntent|applyAskAccessIntent[^;]*\.\s*draft/);
	});

	it('does not call sendMessage from inside the deep-link intent handling', () => {
		// Headline failure mode 5: "the intent populates an empty draft only; nothing is ever
		// published without the user's Send." A grep-level guard that the intent branch does not
		// invoke sendMessage. We find the intent-handling region and assert sendMessage is absent
		// from it. (The send-button handler handleSend/handleComposeSend keep their own calls.)
		const src = chatSrc();
		const intentIdx = src.indexOf("searchParams.get('intent')");
		expect(intentIdx).toBeGreaterThan(-1);
		// Take a generous window around the intent handling (the effect body) and assert no send.
		const region = src.slice(intentIdx, intentIdx + 1200);
		expect(region).not.toMatch(/sendMessage/);
	});

	it('existing draft is not clobbered — the helper result is applied, not a hard overwrite', () => {
		// The pure helper returns the no-clobber decision; the component applies `.draft` from that
		// result rather than unconditionally writing askAccessDraft(...). Pin that the component
		// uses the helper's `.draft` field (the no-clobber path) and not a direct askAccessDraft call.
		const src = chatSrc();
		// The helper is applyAskAccessIntent and the component reads .draft off its return value
		// (inline, destructured, or via `const applied = applyAskAccessIntent(...)` + `applied.draft`).
		expect(src).toMatch(/applyAskAccessIntent\([^)]*\)\.draft|const applied = applyAskAccessIntent|const \{[^}]*draft[^}]*\}\s*=\s*applyAskAccessIntent/);
	});
});
