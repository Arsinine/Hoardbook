// M17 W7.1a — the ask must leave a trace (requester side). Source-scan guard following the repo's
// route-page idiom (ask-access-w2.test.ts, mas-inv5-no-download.test.ts, contacts-w1.test.ts).
//
// Headline failure modes pinned here:
//   - "the ask double-fires" — two clicks inside the cooldown must not silently send two DMs. The
//     "Ask again" button is `disabled={!askState.cooldownOver}`, so the DOM gate is structural.
//   - "a failed ask renders as success" — the asked-state is read back from the persisted record (the
//     record is written server-side AFTER send_dm_inner resolves), and a failure shows the muted
//     reason inline, never "Asked".
//   - "MAS-INV-5" — none of the new copy contains "Download".
//   - the asked-state exposes the W1 chat deep-link so the user goes where the reply will arrive.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { extractUserFacingSegments } from '$lib/copy-audit.js';

const browseSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Browse page — M17 W7.1a the ask leaves a trace', () => {
	it('the asked-state renders from the persisted record (deriveManifestAskState), not optimistic state', () => {
		const src = browseSrc();
		expect(src).toContain('deriveManifestAskState');
		expect(src).toContain('getManifestAsks');
		// The asked-state is keyed by BOTH npub and slug (one entry per pair).
		expect(src).toMatch(/selectedPeer\.npub.*selectedCollection\.slug|selectedPeer\?\.npub.*selectedCollection\?\.slug/s);
	});

	it('handleAskOwner re-reads the persisted map after a successful send (not optimistic)', () => {
		const src = browseSrc();
		expect(src).toContain('async function handleAskOwner');
		// refreshManifestAsks is called in the success path, AFTER requestManifest resolves.
		expect(src).toMatch(/await requestManifest[\s\S]*await refreshManifestAsks/);
	});

	it('a failed publish leaves the un-asked state + shows the muted reason inline (failure is loud)', () => {
		const src = browseSrc();
		// askError is set in the catch branch; rendered only when NOT asked (so a prior successful ask
		// still shows — the failure doesn't hide the truth).
		expect(src).toMatch(/askError = String\(e\)/);
		expect(src).toMatch(/askError && askState\.kind !== 'asked'/);
		expect(src).toContain('MANIFEST_ASK_FAILED_LINE');
	});

	it('the asked-state exposes the "Open chat" deep-link → /chat?peer=<npub> (W1)', () => {
		const src = browseSrc();
		expect(src).toContain('MANIFEST_OPEN_CHAT_LABEL');
		// The link targets W1's chat peer deep-link, where the reply will arrive (it is a DM).
		expect(src).toMatch(/href=\{`\/chat\?peer=\$\{selectedPeer\?\.npub[^}]*\}\`/);
	});

	it('"Ask again" is disabled inside the cooldown (cooldownOver gate, structural)', () => {
		// The double-fire failure mode (a second click silently sending a second DM inside the window)
		// is prevented structurally: the button is disabled when !cooldownOver.
		const src = browseSrc();
		expect(src).toContain('MANIFEST_ASK_AGAIN_LABEL');
		expect(src).toMatch(/disabled=\{askingOwner \|\| !askState\.cooldownOver\}/);
	});

	it('the cooldown tooltip carries the remaining time (MANIFEST_ASK_AGAIN_COOLDOWN_TIP)', () => {
		const src = browseSrc();
		expect(src).toContain('MANIFEST_ASK_AGAIN_COOLDOWN_TIP');
		expect(src).toMatch(/cooldownOver.*MANIFEST_ASK_AGAIN_COOLDOWN_TIP|MANIFEST_ASK_AGAIN_COOLDOWN_TIP.*cooldownRemaining/);
	});

	it('the asked-state line carries the "waiting for their reply" promise', () => {
		const src = browseSrc();
		expect(src).toContain('MANIFEST_ASKED_LINE');
	});

	it('a slow tick keeps the cooldown countdown + relative label honest without a reload', () => {
		// The tick is driven by the shared ASK_TICK_MS constant, not a literal. Both things it
		// refreshes are minute-granular, so a per-second tick would wake the reactive graph 60×
		// per displayable change — the same call W5 made for the contact-row clock.
		const src = browseSrc();
		expect(src).toMatch(/nowTick/);
		expect(src).toMatch(/setInterval\(\(\) => \{ nowTick = Date\.now\(\); \}, ASK_TICK_MS\)/);
		expect(src).toContain('ASK_TICK_MS');
	});

	it('no new user-facing copy contains the forbidden word "Download" (MAS-INV-5)', () => {
		// The invariant sweep must stay green — none of the W7.1a copy introduces "Download".
		const offenders = extractUserFacingSegments(browseSrc())
			.map((seg) => seg.replace(/\bno[\s-]?download\b/gi, ''))
			.filter((seg) => /download/i.test(seg));
		expect(offenders).toEqual([]);
	});

	it('does not remove the existing paywall affordances (ask + import + paste all stay)', () => {
		// The un-asked branch keeps the primary "Ask the owner" and both import affordances exactly as
		// shipped — W7.1a only adds the asked-state branch on top.
		const src = browseSrc();
		expect(src).toContain('Ask the owner for the full list');
		expect(src).toContain('Import a manifest file you received');
		expect(src).toContain('or paste it');
	});

	it('the ask is recorded inside request_manifest (server-side, after send_dm_inner) — api.ts carries the getter', () => {
		// Structural guard: the getter is wired so the route can read the persisted map back. The
		// actual "record after success" invariant is pinned in the Rust store tests; this asserts the
		// frontend has a way to read it.
		const apiSrc = readFileSync(new URL('../../lib/api.ts', import.meta.url), 'utf8');
		expect(apiSrc).toContain('getManifestAsks');
		expect(apiSrc).toContain('get_manifest_asks');
		expect(apiSrc).toContain('interface ManifestAsk');
	});
});
