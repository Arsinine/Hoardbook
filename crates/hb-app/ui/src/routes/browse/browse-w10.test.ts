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
	it('handleImportManifest captures targetNpub and targetSlug before the await', () => {
		// The whole fix hinges on capturing the identity of the peer/collection the import was
		// STARTED for, so the post-await re-check has something to compare against. These two
		// captures must precede the `await importManifest(...)` line.
		const src = browseSrc();
		const fn = src.slice(
			src.indexOf('async function handleImportManifest'),
			src.indexOf('async function pickManifestFile'),
		);
		const awaitIdx = fn.indexOf('await importManifest');
		expect(fn).toContain('const targetNpub = selectedPeer.npub');
		expect(fn).toContain('const targetSlug = selectedCollection.slug');
		expect(fn.indexOf('const targetNpub')).toBeLessThan(awaitIdx);
		expect(fn.indexOf('const targetSlug')).toBeLessThan(awaitIdx);
	});

	it('the success handler re-checks selectedPeer?.npub AND selectedCollection?.slug after the await', () => {
		// The defect's signature is writing into the live view unconditionally after the await. The
		// fix mirrors selectPeer's `if (selectedPeer?.npub === updated.npub)` guard, extended to both
		// the peer and the collection (the import targets a specific collection slug).
		const src = browseSrc();
		const fn = src.slice(
			src.indexOf('async function handleImportManifest'),
			src.indexOf('async function pickManifestFile'),
		);
		const awaitIdx = fn.indexOf('await importManifest');
		// The guard must appear AFTER the await (the re-check is post-await by definition).
		const after = fn.slice(awaitIdx);
		expect(after).toMatch(
			/selectedPeer\?\.npub === targetNpub && selectedCollection\?\.slug === targetSlug/,
		);
		// And the live-view swap (selectedPeer/selectedCollection assignment) must be inside that
		// guarded branch, not flat after the await.
		const guardIdx = after.indexOf('selectedPeer?.npub === targetNpub');
		const swapIdx = after.indexOf('selectedCollection = full');
		expect(swapIdx).toBeGreaterThan(guardIdx);
	});

	it('a stale result folds into the background contacts store instead of clobbering the live view', () => {
		// The else branch of the re-check must NOT touch selectedPeer/selectedCollection — it merges
		// the upgraded listing into the background `contacts` store so a later return to that peer
		// shows the full tree without misattributing it to whoever the user is currently viewing.
		const src = browseSrc();
		const fn = src.slice(
			src.indexOf('async function handleImportManifest'),
			src.indexOf('async function pickManifestFile'),
		);
		// contacts.update appears in the else branch of the guard.
		expect(fn).toMatch(/contacts\.update/);
	});

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
