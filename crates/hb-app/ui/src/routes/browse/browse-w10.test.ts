// M19 W10 — manifest import must not clobber a different peer's view mid-race. Source-scan guard
// following the repo's route-page idiom (ask-trace-w7-1a.test.ts, contacts-w5-dataloss.test.ts,
// contacts-w1.test.ts): the route's onMount fan-out and $app/navigation goto make a full mount heavier
// than the wiring check warrants, so we pin the thing only the page can get wrong.
//
// The bug: `handleImportManifest` captured `targetNpub`/`targetSlug` before `await importManifest(...)`,
// but the success handler wrote into whatever `selectedPeer`/`selectedCollection` was CURRENT when the
// await resolved — no re-check, unlike the sibling `selectPeer` which guards
// `if (selectedPeer?.npub === updated.npub)`. A user who switched peers mid-import got peer A's
// authentically-signed manifest applied under peer B's identity chrome — content misattribution.
//
// The fix: re-check `selectedPeer?.npub === targetNpub && selectedCollection?.slug === targetSlug`
// after the await, mirroring `selectPeer`'s pattern; otherwise fold the result into the background
// `contacts` store only (no clobber of the live view).

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const browseSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Browse page — M19 W10 manifest import does not clobber a switched peer\'s view', () => {
	// ⚠ RETIRED by QURATOR-203 (2026-09-13), not deleted-because-inconvenient.
	//
	// The three tests that stood here scanned `handleImportManifest` for the capture-before-await
	// and the post-await re-check. That function, and the whole manual manifest-import path it
	// belonged to, were REMOVED by owner ruling: fetch and serve are automatic (fetch_driver.rs /
	// auto_approve.rs), so the nine round-trip buttons in the paywall panel were dead chrome.
	//
	// The defect they guarded was real and is worth remembering: an import started for peer A could
	// resolve after the user switched to peer B and be written under B's identity chrome — an
	// authentically-signed manifest shown as the wrong peer's. **If a manual import affordance is
	// ever reintroduced, it must carry the capture-before-await + post-await re-check again**, and
	// these tests should come back with it (recoverable at 2c65b02^).
	//
	// The surviving test below still pins live code: `selectPeer` is the sibling that established
	// the guard shape, and it remains the reference implementation for that pattern.

	it('selectPeer (the established sibling pattern) guards with the same shape — the mirror is faithful', () => {
		// The fix is required to mirror selectPeer. Pin selectPeer's existing guard so a future
		// "cleanup" that weakens it doesn't silently break both the original feature and this mirror.
		const src = browseSrc();
		const selectPeerFn = src.slice(
			src.indexOf('async function selectPeer'),
			src.indexOf('function selectCollection'),
		);
		expect(selectPeerFn).toMatch(/if \(selectedPeer\?\.npub === updated\.npub\) selectedPeer = updated/);
	});
});
