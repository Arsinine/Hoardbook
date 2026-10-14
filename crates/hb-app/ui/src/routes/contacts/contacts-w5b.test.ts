// M21 W5b — group chips carry their colour + the `+` membership popover, guarded at the source.
// The repo's established route-page idiom is source scanning (see contacts-w1, contacts-w2,
// contacts-w4-card, contacts-w5, contacts-w5-dataloss, ask-access-w2, browse-w10): the route's
// onMount fan-out and $app/navigation goto make a full mount heavier than the wiring checks warrant.
//
// Two behaviours lived here because the route wired them:
//   1. Group chips rendered a colour dot from Group.color — RETIRED by QURATOR-378 rev b (owner:
//      "drop groups"); the chips left the pane, and the test below pins their absence.
//   2. A trigger (the pane's `+`, now only the ⋯ menu's "Edit groups…") opens a popover whose Apply sends the FULL checked set to
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

// QURATOR-378 rev b (owner, 2026-10-14: "drop groups"): the pane's group chip row and its `+` are
// GONE, so behaviour 1 (the chips' colour dots) has no surface left on this page — the chips, the
// dot and `groupColor` went with it. The group affordances that remain are the ⋯ menu's "Edit
// groups…" (the popover below) and drag-to-group on the list. This pins the ruling: no chip row and
// no `+` anywhere in the template.
describe('QURATOR-378 rev b — the contact pane carries no group chips', () => {
	it('no group chip row, colour dot or `+` trigger anywhere in the page template', () => {
		// P-10 MUTATION: re-add `<span class="group-pill">{gname}</span>` inside the pane → REDS.
		const s = contactsSrc();
		const template = s
			.slice(0, s.indexOf('<style>'))
			.replace(/<!--[\s\S]*?-->/g, '')
			.replace(/\/\*[\s\S]*?\*\//g, '')
			.replace(/^\s*\/\/.*$/gm, '');
		expect(template).not.toMatch(/class="group-pill"/);
		expect(template).not.toMatch(/class="group-dot"/);
		expect(template).not.toMatch(/class="group-add-btn"/);
		expect(template).not.toMatch(/class="pane-groups"/);
	});
});

describe('M21 W5b behaviour 2 — the membership popover (QURATOR-378 rev b: from the ⋯ menu only)', () => {
	it('the ⋯ menu\'s "Edit groups…" opens the popover via openGroupPopover, selecting that contact', () => {
		// The popover is mounted in the pane for the OPEN contact, so a row's ⋯ must select the
		// contact too, or the popover has no instance to render in.
		// P-10 MUTATION: delete `selectContact(peer.npub); ` from the Edit groups… item → REDS.
		const s = contactsSrc();
		expect(s).toMatch(/function openGroupPopover\(npub: string, anchor: HTMLElement\)/);
		const item = s.slice(s.lastIndexOf('<button', s.indexOf('>Edit groups…</button>')), s.indexOf('>Edit groups…</button>'));
		expect(item).toMatch(/selectContact\(peer\.npub\); if \(menuAnchor\) openGroupPopover\(peer\.npub, menuAnchor\)/);
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
