// M21 W5b — group chips carry their colour + the `+` membership popover, guarded at the source.
// The repo's established route-page idiom is source scanning (see contacts-w1, contacts-w2,
// contacts-w4-card, contacts-w5, contacts-w5-dataloss, ask-access-w2, browse-w10): the route's
// onMount fan-out and $app/navigation goto make a full mount heavier than the wiring checks warrant.
//
// Two behaviours live here only because the route wires them:
//   1. Group chips render a colour dot from Group.color (absent ⇒ no dot, never a broken chip).
//   2. A `+` beside the chip row opens a popover whose Apply sends the FULL checked set to
//      contactUpdateGroups — the same full-set semantics the expanded editor uses. The single-select
//      defect (contacts-w5-dataloss) must not be reintroduced via the popover.
//
// M22 W8 — the popover editor is now the ONE shared GroupMembershipPopover component (used by
// Browse too). The membership-editor assertions that used to scan the route now scan the COMPONENT
// source (the single copy), plus the route's wiring of it. Every adjusted assertion below is proven
// to red on a real regression by a mutation probe (see the m22-drag suites).

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const contactsSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');
const popoverSrc = () =>
	readFileSync(new URL('../../lib/components/GroupMembershipPopover.svelte', import.meta.url), 'utf8');

describe('M21 W5b behaviour 1 — group chips carry their colour dot', () => {
	it('a groupColor helper looks up Group.color by name', () => {
		const s = contactsSrc();
		expect(s).toMatch(/function groupColor\(name: string\)/);
		// Reads the colour off the same `groups` array that seeds the editor — not re-derived.
		expect(s).toMatch(/groups\.find\(g => g\.name === name\)\?\.color/);
	});

	// QURATOR-368c RE-POINT: the chip row moved off the face into the pane's `.pane-groups` block —
	// the dot logic is unchanged, so the slice follows it there.
	it('the chip renders a dot when the group has a colour', () => {
		const s = contactsSrc();
		// Inside the pane-groups chip loop: a `.group-dot` whose background is the looked-up color.
		const groupsIdx = s.indexOf('class="pane-groups"');
		const regionEnd = s.indexOf('class="detail-block', groupsIdx);
		const chipRegion = s.slice(groupsIdx, regionEnd);
		expect(chipRegion).toMatch(/groupColor\(gname\)/);
		expect(chipRegion).toMatch(/class="group-dot"/);
		expect(chipRegion).toMatch(/background:\$\{gcolor\}/);
	});

	// INVERTED twice. This test was 'the detail chip renders the same dot (face and detail agree)';
	// devtest 2026-08-26 item 3 ruling 01 deleted the DETAIL row ("showing duplicated information")
	// and made the face the only one; QURATOR-368c moved the row back into the pane. The property
	// that has needed guarding through both flips: exactly ONE chip row exists — the face carries
	// none, the pane carries one. A second chip row anywhere reds here.
	it('the chip row lives in the pane ONLY — the face carries no second copy', () => {
		const s = contactsSrc();
		const detailStart = s.indexOf('class="detail-pane');
		expect(detailStart).toBeGreaterThan(-1);
		const face = s
			.slice(s.indexOf('class="contact-card"'), detailStart)
			// Comments only: a tombstone naming what moved may survive (CLAUDE.md §9 — what must not
			// appear is the affordance, not the word).
			.replace(/<!--[\s\S]*?-->/g, '')
			.replace(/^\s*\/\/.*$/gm, '');
		expect(face).not.toMatch(/groupColor\(gname\)/);
		expect(face).not.toMatch(/class="group-dot"/);
		// …and the pane has it, so this cannot pass by the chips having vanished entirely.
		const groupsIdx = s.indexOf('class="pane-groups"');
		const regionEnd = s.indexOf('class="detail-block', groupsIdx);
		const paneGroups = s.slice(groupsIdx, regionEnd);
		expect(paneGroups).toMatch(/groupColor\(gname\)/);
		expect(paneGroups).toMatch(/class="group-dot"/);
		// Exactly one chip row in the whole template.
		expect((s.match(/class="group-pill"/g) ?? []).length).toBe(1);
	});

	it('a group with no colour renders the chip with NO dot (guarded by {#if gcolor})', () => {
		// The colour is always read into a const and the dot is gated on it being present — so a
		// no-colour group renders the chip text with no dot, never a broken/invisible chip.
		const s = contactsSrc();
		expect(s).toMatch(/\{@const gcolor = groupColor\(gname\)\}/);
		expect(s).toMatch(/\{#if gcolor\}<span class="group-dot"/);
	});

	it('the dot has a dedicated style rule (not a re-used pill-dot)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/\.group-dot\s*\{/);
	});
});

describe('M21 W5b behaviour 2 — `+` opens the membership popover (QURATOR-368c: from the pane)', () => {
	it('the pane renders a `+` control beside the chip row', () => {
		const s = contactsSrc();
		const groupsIdx = s.indexOf('class="pane-groups"');
		const regionEnd = s.indexOf('class="detail-block', groupsIdx);
		const chipRegion = s.slice(groupsIdx, regionEnd);
		expect(chipRegion).toMatch(/class="group-add-btn"/);
		expect(chipRegion).toMatch(/>\+</);
	});

	it('the `+` opens the popover via openGroupPopover (anchored, aria-haspopup)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/function openGroupPopover\(npub: string, anchor: HTMLElement\)/);
		expect(s).toMatch(/aria-haspopup="true"/);
		expect(s).toMatch(/aria-expanded=\{groupPopoverFor === peer\.npub\}/);
		expect(s).toMatch(/openGroupPopover\(peer\.npub, e\.currentTarget\)/);
	});

	it('the popover seeds its draft from CURRENT memberships (pre-check loses nothing)', () => {
		// The data-loss guard the expanded editor enforces — seed from CURRENT memberships, not empty.
		// M22 W8: the seed now happens in the SHARED component (`draft = new Set(memberships)`), and
		// the route feeds it the contact's current memberships (`memberships={contactGroups(...)}`).
		const p = popoverSrc();
		expect(p).toMatch(/draft = new Set\(memberships\)/);
		const s = contactsSrc();
		expect(s).toMatch(/memberships=\{contactGroups\(peer\.npub\)\}/);
	});

	it('Apply sends the WHOLE checked set to contactUpdateGroups, not a single name', () => {
		// The single-select defect (contacts-w5-dataloss) must not return via the popover. M22 W8:
		// the component's commit() spreads the WHOLE draft set through onapply; the route's apply
		// handler forwards that set verbatim to contactUpdateGroups — the same full-set command the
		// expanded editor uses. Neither path may build a single-element array.
		const p = popoverSrc();
		expect(p).toMatch(/onapply\(\[\.\.\.draft\]\)/);
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('async function applyGroupPopover'), s.indexOf('// M20 W2:'));
		expect(fn).toMatch(/contactUpdateGroups\(npub, names\)/);
		// The route must NOT have re-introduced a single-name derivation anywhere near the popover.
		expect(fn).not.toMatch(/\[newGroupName\]/);
	});

	it('Cancel discards the draft (closes without calling contactUpdateGroups)', () => {
		const s = contactsSrc();
		// M22 W8: the route's onclose just nulls the open state — no save. The component's Cancel
		// button routes to the same onclose. The discard is the absence of a contactUpdateGroups call
		// in the close path.
		expect(s).toMatch(/onclose=\{\(\) => \(groupPopoverFor = null\)\}/);
		const p = popoverSrc();
		expect(p).toMatch(/onclick=\{close\}/); // Cancel button calls close(), not commit()
		expect(p).toMatch(/function close\(\) \{\s*onclose\(\);/);
	});

	it('the popover offers "+ New group…" routed to the existing CreateGroupDialog', () => {
		// M22 W8: the component renders the "+ New group…" control; the route wires it to its OWN
		// create dialog (onnewgroup → createGroupOpen = true). Both halves must be present.
		const p = popoverSrc();
		expect(p).toMatch(/\+ New group…/);
		expect(p).toMatch(/onclick=\{onnewgroup\}/);
		const s = contactsSrc();
		expect(s).toMatch(/onnewgroup=\{\(\) => \(createGroupOpen = true\)\}/);
	});

	it('the popover reuses the OverflowMenu shell (anchored, Escape-to-close), not a new surface', () => {
		// M22 W8: no new positioning code — the SHARED component is a single OverflowMenu with
		// membership contents. The route just hands it the anchor + open state.
		const p = popoverSrc();
		expect(p).toMatch(/OverflowMenu/);
		expect(p).toMatch(/<OverflowMenu \{open\} \{anchor\} onclose=\{close\}/);
		const s = contactsSrc();
		expect(s).toMatch(/anchor=\{groupPopoverAnchor\}/);
		expect(s).toMatch(/open=\{groupPopoverFor === peer\.npub\}/);
	});

	it('the popover is now the ONLY group editor — the expanded panel no longer carries one', () => {
		// INVERTED 2026-08-27 by devtest item 3, ruling 01. When W5b landed, the popover was added as
		// "an additional route, not a replacement", and this test pinned the expanded panel's editor
		// (beginGroupEdit / handleSaveGroups) as still present. The owner then reported the expanded
		// panel as showing duplicated information — the panel's chip row duplicated the card face's,
		// and its checkbox editor duplicated this popover. So the panel's editor became the thing that
		// must NOT exist, and W5b's popover became the replacement it originally declined to be.
		//
		// This is an inversion, not a deletion: the full-set write semantics both editors shared are
		// still pinned, in contacts-w5-dataloss.test.ts, now against the popover.
		// Comments are stripped first: the deletion left tombstones NAMING these symbols so the next
		// reader knows where the editor went, and an absence assertion must not red on its own
		// explanation (CLAUDE.md §9 — the affordance, not the word).
		const s = contactsSrc();
		const code = s
			.replace(/<!--[\s\S]*?-->/g, '')
			.replace(/\/\*[\s\S]*?\*\//g, '')
			.replace(/^\s*\/\/.*$/gm, '');
		expect(code).not.toMatch(/function beginGroupEdit\(/);
		expect(code).not.toMatch(/async function handleSaveGroups\(/);
		expect(code).not.toMatch(/contactGroupDraft\[/);
		// The surviving editor's entry points, both of them, still exist.
		expect(s).toMatch(/function openGroupPopover\(npub: string, anchor: HTMLElement\)/);
		expect(s).toMatch(/async function applyGroupPopover\(npub: string, names: string\[\]\)/);
	});
});
