// M20 W5 (data-loss half) — the per-contact group editor must be multi-select, not single-select.
// The repo's established pattern for route-page guards is source scanning (see contacts-w1, contacts-w2,
// the M17 contacts-w5, mas-inv5-no-download): the route's onMount fan-out and $app/navigation goto make
// a full mount heavier than the wiring checks warrant, so we pin the thing only the page can get wrong.
//
// The bug: the data model + renderer are many-to-many, but the editor sent
// `const groupNames = newGroupName ? [newGroupName] : []` — a single-element set — to
// `contactUpdateGroups`, whose backend diffs the FULL set (commands/groups.rs contact_update_groups).
// Choosing group B silently removed the contact from group A. Live data loss.
//
// The fix: the editor shows ALL groups as checkboxes (pre-checked with current memberships) and sends
// the complete checked set. No path may send a single-element set as the full membership.
//
// 2026-08-27, devtest item 3 ruling 01: there used to be TWO editors — one in the expanded detail
// panel, one in the collapsed card's popover. The panel's was deleted (it duplicated the face's
// group chips, which is what the owner reported). The assertions below were RETARGETED onto the
// surviving GroupMembershipPopover rather than dropped: the data loss they pin is a property of
// the WRITE, not of the surface, and deleting a guard because its subject moved is how a
// regression walks back in.

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const contactsSrc = () =>
	readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');
const popoverSrc = () =>
	readFileSync(new URL('../../lib/components/GroupMembershipPopover.svelte', import.meta.url), 'utf8');

/** contacts/+page.svelte with comments removed. Every absence assertion below runs against THIS, not
 *  the raw file: the deleted editor left tombstone comments naming `.group-edit-list` and
 *  `handleSaveGroups` so a future reader knows why they are gone, and a raw-text `not.toMatch` would
 *  red on the explanation instead of on the code. What must not exist is the affordance, not the
 *  word (CLAUDE.md §9). */
const contactsCode = () =>
	contactsSrc()
		.replace(/<!--[\s\S]*?-->/g, '')
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/^\s*\/\/.*$/gm, '');

describe('contacts M20-W5 — group editor is multi-select (no silent membership drop)', () => {
	it('does NOT send a single-group array as the full membership set', () => {
		// The defect: `const groupNames = newGroupName ? [newGroupName] : []` collapsed membership
		// to one group, so the backend's full-set diff removed every other membership.
		const s = contactsSrc();
		expect(s).not.toMatch(/const groupNames = newGroupName \? \[newGroupName\] : \[\]/);
	});

	it('the old single-select handleMoveGroup(hb_id, newGroupName) signature is gone', () => {
		// The old handler took one `newGroupName` and built a 1-element (or empty) array. The fix
		// sends the full checked set under a different signature. The old one must not survive.
		const s = contactsSrc();
		expect(s).not.toMatch(/async function handleMoveGroup\(hb_id: string, newGroupName: string\)/);
	});

	it('the editor renders checkboxes over ALL groups, pre-checked with current memberships', () => {
		// RETARGETED by devtest 2026-08-26 item 3 (ruling 01), NOT weakened. This used to read the
		// route's own `checked={peerGroups.includes(g.name)}` inside the detail panel's editor; that
		// editor was deleted so the page has exactly one group editor instead of two. The invariant is
		// unchanged and now lives entirely in the shared component: a checkbox PER GROUP, checked from
		// the draft that was seeded off current memberships. Nothing about the data loss this file
		// exists to pin depends on which surface draws the checkbox.
		const p = popoverSrc();
		// One checkbox for every group the page knows — the `{#each groups}` is what makes it a
		// multi-select rather than a pick-one.
		expect(p).toMatch(/\{#each groups as g \(g\.name\)\}/);
		expect(p).toMatch(/type="checkbox"[\s\S]{0,120}?checked=\{draft\.has\(g\.name\)\}/);
		// …and the draft it reads is seeded from the CURRENT memberships, which is the half that
		// stops the pre-check from silently dropping an existing group.
		expect(p).toMatch(/draft = new Set\(memberships\)/);
		// The deleted editor must not come back as a third surface.
		expect(contactsCode()).not.toMatch(/group-edit-list/);
	});

	it('saving sends the complete checked set to contactUpdateGroups, not a single name', () => {
		// RETARGETED by devtest item 3 alongside the test above: the save path used to be the panel
		// editor's `handleSaveGroups`, which built its set as `[...(contactGroupDraft[hb_id] ?? [])]`.
		// With that editor deleted, the ONE save path is the popover's commit → the route's
		// applyGroupPopover. The assertion is the same one: whatever reaches contactUpdateGroups is a
		// spread of the whole draft, never a one-element array.
		const s = contactsSrc();
		expect(s).not.toMatch(/const groupNames = newGroupName \? \[newGroupName\] : \[\]/);
		// Component side: commit spreads the ENTIRE draft.
		expect(popoverSrc()).toMatch(/onapply\(\[\.\.\.draft\]\)/);
		// Route side: it forwards that array verbatim — no filtering, no picking one name out.
		const fn = s.slice(s.indexOf('async function applyGroupPopover'), s.indexOf('// M20 W2:'));
		expect(fn).toMatch(/contactUpdateGroups\(npub, names\)/);
		expect(fn).not.toMatch(/names\[0\]|\[names\b/);
		// And the deleted panel editor's handler must not be resurrected as a second writer.
		expect(contactsCode()).not.toMatch(/async function handleSaveGroups\(/);
	});

	// M21 W5b / M22 W8: the collapsed-card `+` popover is a SECOND route to the same command. It must
	// inherit the same full-set semantics — single-select via the popover would reintroduce the exact
	// data-loss this test exists to pin. The popover is now the ONE shared GroupMembershipPopover
	// component: its commit() builds the set from its OWN draft (seeded from current memberships) and
	// spreads the WHOLE set through onapply; the route's apply handler forwards that set verbatim to
	// contactUpdateGroups. Neither half may build a single-element array.
	it('the popover Apply path (applyGroupPopover) also sends the full checked set, not one name', () => {
		const p = popoverSrc();
		// The component seeds the draft from the CURRENT memberships prop and spreads the WHOLE set.
		expect(p).toMatch(/draft = new Set\(memberships\)/);
		expect(p).toMatch(/onapply\(\[\.\.\.draft\]\)/);
		// The route feeds the component the current memberships and forwards the set to the command.
		const s = contactsSrc();
		expect(s).toMatch(/memberships=\{contactGroups\(peer\.npub\)\}/);
		const fn = s.slice(s.indexOf('async function applyGroupPopover'), s.indexOf('// M20 W2:'));
		expect(fn).toMatch(/contactUpdateGroups\(npub, names\)/);
		// No single-element-set derivation may survive anywhere in the popover path.
		expect(fn).not.toMatch(/\[newGroupName\]/);
		expect(p).not.toMatch(/\[newGroupName\]/);
	});
});
