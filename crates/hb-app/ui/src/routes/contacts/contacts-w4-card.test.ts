// M21 W4 — the contact-card redesign, guarded at the source. The page's heavy onMount fan-out +
// `$app/navigation` goto make a full mount heavier than the wiring checks warrant, so we pin the
// seven locked behaviours against the route source — the repo's established route-page idiom
// (see contacts-w1, contacts-w2, contacts-w5, contacts-w5-dataloss, ask-access-w2). The pure
// helper (fingerprint-colour table) has its OWN DOM-free unit test in lib/fingerprint-colors.test.ts.

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const contactsSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('M21 W4 behaviour 1 — the npub is OFF the card face; survives in ⋯ and detail', () => {
	it('the face no longer renders the npub via shortId/shortId-equivalent inline mono', () => {
		const s = contactsSrc();
		// The old face had `<div class="mono">{shortId(peer.npub)}</div>` — both the helper and the
		// markup it fed are gone.
		expect(s).not.toMatch(/class="mono"/);
		expect(s).not.toMatch(/shortId\(peer\.npub\)/);
		// The shortId helper itself is removed (no remaining callers after the face dropped it).
		expect(s).not.toMatch(/function shortId/);
	});

	it('the ⋯ overflow menu carries a Copy npub item (the npub\'s surviving home on the row)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/Copy npub/);
		expect(s).toMatch(/copyNpub\(peer\.npub\)/);
	});

	it('the pill takes the slot the npub was holding (presence pill still on the face)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/class="pill pill-online"/);
		// QURATOR-368: Offline is said ONCE, as line-2 text carrying the state class — not a pill.
		expect(s).toMatch(/class="last-seen pill-offline"/);
	});
});

describe('M21 W4 behaviour 2 — presence + cache-age are INDEPENDENT (no Stale pill)', () => {
	it('the Stale pill branch is gone (a stale contact no longer stops reporting online/offline)', () => {
		const s = contactsSrc();
		// The old three-branch pill (online / stale / offline) collapsed to two — Stale no longer
		// replaces Offline. The pill ALWAYS answers "are they online".
		expect(s).not.toMatch(/\{:else if stale\}/);
		expect(s).not.toMatch(/pill-stale/);
		expect(s).not.toMatch(/>Stale</);
	});

	// QURATOR-368c RE-POINT: the control moved off the row into the pane's actions (the row at rest
	// is two lines), so the const is the pane's `paneColdCache` — the gating condition is unchanged.
	it('the cold-cache control renders ONLY when isStale && !online, in the pane actions', () => {
		const s = contactsSrc();
		expect(s).toMatch(/\{@const paneColdCache = isStale\(peer\) && !peer\.online\}/);
		// The control carries the ↻ glyph and the checked-age label, wired to handleRefresh.
		expect(s).toMatch(/class="cold-cache"/);
		expect(s).toMatch(/handleRefresh\(peer\.npub\)/);
		expect(s).toMatch(/checkedLabel\(peer\.last_fetched, nowMs\)\} ↻/);
	});

	it('the cold-cache control uses the reactive clock (nowMs), not Date.now()', () => {
		// Reading Date.now() in the template froze the age when a poll failed (the original "seen just
		// now forever" defect the W5 clock fixed). The cold-cache control must inherit that fix.
		const s = contactsSrc();
		const idx = s.indexOf('class="cold-cache"');
		const region = s.slice(idx, idx + 200);
		expect(region).toContain('nowMs');
		expect(region).not.toContain('Date.now()');
	});

	it('the warm-cache `checked {t}` always-on line is gone (only cold cache shows it)', () => {
		const s = contactsSrc();
		// The old `<span class="last-checked">{checkedLabel(...)}</span>` rendered on EVERY card; W4
		// renders the age only when the cache is cold. The always-on class is gone.
		expect(s).not.toMatch(/class="last-checked"/);
	});
});

describe('M21 W4 behaviour 3 — coloured fingerprint from the fixed word→hue table', () => {
	it('the face renders the Rust-selected words via fingerprintWordColor, not re-derived', () => {
		const s = contactsSrc();
		expect(s).toMatch(/import \{ fingerprintWordColor \}/);
		expect(s).toMatch(/fingerprintWordColor\(w\)/);
		// The {#each fp.words} loop is the Rust-selected set, unmodified (5 words since QURATOR-121
		// #24; no slicing/truncating back to the old width).
		expect(s).not.toMatch(/fp\.words\.slice\(0, \d\)|fp\.words\.concat/);
	});

	it('the fixed table lives in a pure helper module, not in the component', () => {
		// The 16-word→hue table is a pure, unit-tested helper (lib/fingerprint-colors.ts), not inline.
		expect(contactsSrc()).not.toMatch(/amber.*oklch.*basalt.*oklch/s);
	});
});

describe('M21 W4 behaviour 4 — avatar ring from colorHex; no-fingerprint renders neither', () => {
	it('the avatar ring uses fingerprint.colorHex with the two-layer box-shadow', () => {
		const s = contactsSrc();
		// box-shadow: 0 0 0 2px var(--bg-elev1), 0 0 0 4px {colorHex}
		expect(s).toMatch(/box-shadow: 0 0 0 2px var\(--bg-elev1\), 0 0 0 4px \$\{fp\.colorHex\}/);
	});

	it('a contact with no fingerprint renders NEITHER ring NOR word row (guarded by {#if fp})', () => {
		const s = contactsSrc();
		// Both the ring (style={fp ? ...}) and the word row ({#if fp}) are gated on fp being present.
		expect(s).toMatch(/style=\{fp \?/);
		expect(s).toMatch(/\{#if fp\}/);
	});
});

describe('M21 W4 behaviour 5 — petnameFor collision warning on the card face', () => {
	it('the face calls petnameFor and renders its .warning as a red-outlined badge', () => {
		const s = contactsSrc();
		expect(s).toMatch(/import \{ petnameFor \}/);
		expect(s).toMatch(/petnameFor\(peer\.npub, name, contactsForLabel\)\.warning/);
		// The badge is a distinct element (not a re-used pill), red-outlined via --error.
		expect(s).toMatch(/class="collision-badge"/);
		expect(s).toMatch(/var\(--error\)/);
	});

	it('the collision badge renders next to the name (before the presence pill)', () => {
		const s = contactsSrc();
		const nameIdx = s.indexOf('class="peer-name"');
		const pillIdx = s.indexOf('class="pill pill-online"');
		const collisionIdx = s.indexOf('class="collision-badge"');
		expect(nameIdx).toBeGreaterThan(-1);
		expect(collisionIdx).toBeGreaterThan(nameIdx);
		expect(pillIdx).toBeGreaterThan(collisionIdx);
	});
});

describe('M21 W4 behaviour 6 — N collections hoverable, public ONLY', () => {
	it('the face count filters on visibility !== Private (private excluded from the number)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/peer\.collections\.filter\(c => c\.visibility !== 'Private'\)/);
	});

	it('the popover lists the SAME filtered set (number and list cannot disagree)', () => {
		const s = contactsSrc();
		// Both the count and the {#each} read `publicCollections` — the same derived value.
		expect(s).toMatch(/\{@const publicCollections/);
		expect(s).toMatch(/{#each publicCollections as col/);
	});

	it('the list carries path_alias + est_size per row, and a "public collections only" footer', () => {
		const s = contactsSrc();
		expect(s).toMatch(/col\.path_alias/);
		expect(s).toMatch(/col\.est_size/);
		// QURATOR-368c: the popover became a plain pane list; the disclosure footer survives it
		// (the old copy pointed at an "expanded card" that no longer exists).
		expect(s).toMatch(/Public collections only\. Private ones sealed to you are listed separately/);
	});

	// QURATOR-368c RE-POINT: this test pinned the HOVER POPOVER (a ▼ caret trigger opened on
	// :hover/:focus-within). That premise is gone — the list moved into the right pane as a plain
	// always-rendered block, so there is no trigger and nothing to keyboard-reveal. What must hold
	// now: the list exists ONLY as the pane block, and no popover chrome survived anywhere.
	it('the collections list is a plain pane block — no hover popover chrome survived', () => {
		const s = contactsSrc();
		expect(s).toMatch(/class="pane-cols"/);
		expect(s).toMatch(/\{#each publicCollections as col/);
		// The old affordance is gone outright (P-12: the affordance must not appear, prose aside).
		expect(s).not.toMatch(/collections-popover/);
		expect(s).not.toMatch(/collections-trigger/);
	});
});

describe('QURATOR-368c — the bio lives in the pane, full; the face carries none', () => {
	// The detail region is `.contact-detail` (from its opening div to the OverflowMenu that follows
	// the contact-block). Slicing on `{/if}` is wrong — the detail has nested {#if}s, so the first
	// `{/if}` closes a nested branch, not the detail. Slice on the next structural landmark instead.
	function detailRegion(s: string): string {
		const start = s.indexOf('class="contact-detail"');
		const end = s.indexOf('OverflowMenu', start);
		return s.slice(start, end);
	}

	it('the detail renders the FULL bio (`.pane-bio`), and no face-clamp paragraph exists anywhere', () => {
		const s = contactsSrc();
		// QURATOR-368c: the face bio AND its 2-line clamp moved out — the pane shows the whole bio
		// unclamped. The old `<p class="card-bio">` must not come back anywhere (face or pane).
		expect(detailRegion(s)).toMatch(/class="pane-bio"/);
		expect(s).not.toMatch(/<p class="card-bio">/);
		// No clamp machinery left: the pane bio is never truncated.
		expect(s).not.toMatch(/line-clamp/);
	});

	// QURATOR-368c RE-POINT: this test pinned the face's `more ⌄` control and its state wiring
	// (bioExpanded / toggleBio, plus selectContact's collapse-on-open reset). That premise is gone —
	// the face bio was deleted outright, so the clamp state has nothing left to control. What must
	// hold now: the helpers are DELETED, not merely unused, and selectContact carries no residue.
	it('the face clamp state is deleted outright — no helpers, no `more ⌄`, no selectContact residue', () => {
		const s = contactsSrc();
		expect(s).not.toMatch(/function toggleBio/);
		expect(s).not.toMatch(/bioExpanded/);
		expect(s).not.toMatch(/bioOverflowMap/);
		expect(s).not.toMatch(/more ⌄/);
		// selectContact survived (slice b) but sheds the bio reset with the clamp.
		const fn = s.slice(s.indexOf('function selectContact'), s.indexOf('async function copyNpub'));
		expect(fn).not.toMatch(/bioExpanded/);
	});

	// M23 W6's measurement machinery (bioMeasure + bioOverflows) existed to feed the `more ⌄` gate.
	// With the clamp gone there is nothing to measure, so the wiring must be gone too — including
	// the import. (The pure function itself stays in lib/ with its own unit test; it is just no
	// longer used by this page.)
	it('the bio measurement wiring is gone from the page (nothing left to measure)', () => {
		const s = contactsSrc();
		expect(s).not.toMatch(/bioMeasure/);
		expect(s).not.toMatch(/bioOverflows/);
		expect(s).not.toMatch(/bio-overflow\.js/);
		expect(s).not.toMatch(/scrollHeight/);
		expect(s).not.toMatch(/ResizeObserver/);
	});

	it('the private-collections section STILL lives in the detail (the load-bearing reason chevron survives)', () => {
		const s = contactsSrc();
		const detail = detailRegion(s);
		expect(detail).toMatch(/privateByAuthor\[peer\.npub\]/);
		expect(detail).toMatch(/CollectionPanel/);
	});
});

describe('M21 W4 — no bio duplication, no npub on the face', () => {
	// QURATOR-368c RE-POINT (premise inverted): the face used to hold the ONLY bio (clamped) and
	// the detail none; the ruling moved the bio into the pane. The property that survives is
	// "exactly one bio render, and it is in the pane" — the face carries none.
	it('the card face carries no bio; the pane holds exactly one', () => {
		const s = contactsSrc();
		const faceStart = s.indexOf('class="contact-card"');
		const faceEnd = s.indexOf('class="detail-pane');
		const face = s.slice(faceStart, faceEnd);
		expect(face).not.toMatch(/pane-bio/);
		expect(face).not.toMatch(/card-bio/);
		expect(face).not.toMatch(/No bio published/);
		// …and the pane still has it, so this cannot pass by the bio having vanished entirely.
		const start = s.indexOf('class="contact-detail"');
		const end = s.indexOf('OverflowMenu', start);
		const detail = s.slice(start, end);
		expect(detail.match(/class="pane-bio"/g)?.length).toBe(1);
	});

	it('no bio publishes a dim "No bio published." with no `more ⌄` control', () => {
		const s = contactsSrc();
		expect(s).toMatch(/No bio published\./);
	});
});
