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
		expect(s).toMatch(/class="pill pill-offline"/);
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

	it('the cold-cache marker renders ONLY when isStale && !online, as an amber control in the meta line', () => {
		const s = contactsSrc();
		expect(s).toMatch(/\{@const coldCache = isStale\(peer\) && !peer\.online\}/);
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

	it('the popover carries path_alias + est_size per row, and a "public collections only" footer', () => {
		const s = contactsSrc();
		expect(s).toMatch(/col\.path_alias/);
		expect(s).toMatch(/col\.est_size/);
		expect(s).toMatch(/public collections only — private ones live in the detail/);
	});

	it('the trigger shows a ▼ caret signalling hover, and :focus-within makes it keyboard-reachable', () => {
		const s = contactsSrc();
		expect(s).toMatch(/collections-trigger.*▼/s);
		expect(s).toMatch(/:focus-within .collections-popover/);
		expect(s).toMatch(/tabindex="0"/);
	});
});

describe('M21 W4 behaviour 7 — the detail loses ONLY its bio paragraph', () => {
	// The detail region is `.contact-detail` (from its opening div to the OverflowMenu that follows
	// the contact-block). Slicing on `{/if}` is wrong — the detail has nested {#if}s, so the first
	// `{/if}` closes a nested branch, not the detail. Slice on the next structural landmark instead.
	function detailRegion(s: string): string {
		const start = s.indexOf('class="contact-detail"');
		const end = s.indexOf('OverflowMenu', start);
		return s.slice(start, end);
	}

	it('the detail\'s own <p class="card-bio"> is gone (the face clamp replaced it)', () => {
		const s = contactsSrc();
		// The bio paragraph used to sit at the top of the detail; it's now removed, leaving
		// content_types/tags/groups/local-tags/audience/private-collections.
		expect(detailRegion(s)).not.toMatch(/<p class="card-bio">/);
	});

	it('the face bio has a `more ⌄` control that disappears once the detail is open', () => {
		const s = contactsSrc();
		// The toggleBio control exists, and opening the detail clears bioExpanded (so the `more ⌄`
		// hides when the detail's full bio takes over — two controls never reveal the same text).
		expect(s).toMatch(/function toggleBio/);
		expect(s).toMatch(/more ⌄/);
		const fn = s.slice(s.indexOf('function toggleDetail'), s.indexOf('async function copyNpub'));
		expect(fn).toMatch(/if \(opening && bioExpanded\[npub\]\)/);
		// The clamp lifts when EITHER the user clicked `more ⌄` OR the detail is open — so the face
		// bio shows in full as the detail's replacement, and the two controls never double-reveal.
		expect(s).toMatch(/class:bio-expanded=\{bioOpen \|\| isOpen\}/);
		// The control hides when the detail is open. (M23 W6 also ANDs the measured-overflow map onto
		// this same {#if}; that condition is pinned in the dedicated W6 test below.)
		expect(s).toMatch(/\{#if !bioOpen && !isOpen && bioOverflowMap/);
	});

	// M23 W6 — the `more ⌄` control must only appear when the bio ACTUALLY overflows the 2-line clamp,
	// not for every bio. This is a real layout measurement (scrollHeight > clientHeight), re-evaluated
	// on resize — a length heuristic is wrong and was explicitly rejected. The DOM measurement itself
	// lives in the `bioMeasure` action and cannot be exercised in jsdom (scrollHeight is 0 there); the
	// pure comparison seam is pinned in lib/bio-overflow.test.ts. Here we guard the WIRING: that the
	// control's {#if} carries the overflow condition, and that the measurement is real (not a count).
	it('the `more ⌄` control is gated on a measured overflow, not just a bio existing', () => {
		const s = contactsSrc();
		// The control's {#if} must reference the per-npub overflow map — without it a one-line bio
		// would still render `more ⌄` and expand to the identical text.
		expect(s).toMatch(/\{#if !bioOpen && !isOpen && bioOverflowMap\[peer\.npub\]\}/);
		// The measurement reads scrollHeight/clientHeight (real layout), not .length or a char count.
		expect(s).toMatch(/scrollHeight/);
		expect(s).toMatch(/clientHeight/);
		expect(s).not.toMatch(/bio.*\.length\s*[<>]/);
		// The measurement re-evaluates on resize so a width change that un-wraps the bio drops it.
		expect(s).toMatch(/ResizeObserver/);
		// The pure comparison seam is imported (the testable half of the split).
		expect(s).toMatch(/import \{ bioOverflows \} from '\$lib\/bio-overflow\.js'/);
	});

	it('the private-collections section STILL lives in the detail (the load-bearing reason chevron survives)', () => {
		const s = contactsSrc();
		const detail = detailRegion(s);
		expect(detail).toMatch(/privateByAuthor\[peer\.npub\]/);
		expect(detail).toMatch(/CollectionPanel/);
	});
});

describe('M21 W4 — no bio paragraph duplication, no npub on the face', () => {
	it('the card face has exactly one bio region (the clamped face bio), the detail has none', () => {
		const s = contactsSrc();
		// The face bio sits in the .bio-row region; the detail region has no <p class="card-bio">.
		const start = s.indexOf('class="contact-detail"');
		const end = s.indexOf('OverflowMenu', start);
		expect(s.slice(start, end)).not.toMatch(/<p class="card-bio">/);
		expect(s).toMatch(/class="bio-row/);
	});

	it('no bio publishes a dim "No bio published." with no `more ⌄` control', () => {
		const s = contactsSrc();
		expect(s).toMatch(/No bio published\./);
	});
});
