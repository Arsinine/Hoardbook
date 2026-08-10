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

	it('the face chip renders a dot when the group has a colour', () => {
		const s = contactsSrc();
		// Inside the contact-sub-row chip loop: a `.group-dot` whose background is the looked-up color.
		const subRowIdx = s.indexOf('class="contact-sub-row"');
		const detailIdx = s.indexOf('class="contact-detail"');
		const faceRegion = s.slice(subRowIdx, detailIdx);
		expect(faceRegion).toMatch(/groupColor\(gname\)/);
		expect(faceRegion).toMatch(/class="group-dot"/);
		expect(faceRegion).toMatch(/background:\$\{gcolor\}/);
	});

	it('the detail chip renders the same dot (face and detail agree)', () => {
		const s = contactsSrc();
		const detailStart = s.indexOf('class="contact-detail"');
		const detailEnd = s.indexOf('OverflowMenu', detailStart);
		const detail = s.slice(detailStart, detailEnd);
		// The non-editing chip branch inside the detail also renders the dot from groupColor.
		expect(detail).toMatch(/groupColor\(gname\)/);
		expect(detail).toMatch(/class="group-dot"/);
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

describe('M21 W5b behaviour 2 — `+` opens a membership popover on the collapsed card', () => {
	it('the face renders a `+` control beside the chip row', () => {
		const s = contactsSrc();
		const subRowIdx = s.indexOf('class="contact-sub-row"');
		const detailIdx = s.indexOf('class="contact-detail"');
		const faceRegion = s.slice(subRowIdx, detailIdx);
		expect(faceRegion).toMatch(/class="group-add-btn"/);
		expect(faceRegion).toMatch(/>\+</);
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

	it('the expanded checkbox editor still works (an additional route, not a replacement)', () => {
		// The existing beginGroupEdit/handleSaveGroups path is untouched.
		const s = contactsSrc();
		expect(s).toMatch(/function beginGroupEdit\(hb_id: string\)/);
		expect(s).toMatch(/async function handleSaveGroups\(hb_id: string\)/);
	});
});
