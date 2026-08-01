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

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const contactsSrc = () =>
	readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

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
		// The non-negotiable: a multi-select checkbox editor inside the contactGroupEditing branch.
		// The old single `<select>` had `selected={peerGroups.includes(g.name)}` on an <option>; the
		// fix moves that same membership test onto a checkbox `checked`. Assert a checkbox whose
		// checked state is driven by `peerGroups.includes(g.name)` — that shape only exists in the
		// editor, so it can't be satisfied today by the trusted-groups strip or the old select.
		const s = contactsSrc();
		expect(s).toMatch(/type="checkbox"[^>]*checked=\{peerGroups\.includes\(g\.name\)\}|checked=\{peerGroups\.includes\(g\.name\)\}[^>]*type="checkbox"/);
	});

	it('saving sends the complete checked set to contactUpdateGroups, not a single name', () => {
		// The defect built the set from one name: `const groupNames = newGroupName ? [newGroupName] : []`.
		// The fix builds it from the checkbox draft: `[...(contactGroupDraft[hb_id] ?? [])]`. Assert
		// the old single-name derivation is gone, and the call is fed by the draft set.
		const s = contactsSrc();
		expect(s).not.toMatch(/const groupNames = newGroupName \? \[newGroupName\] : \[\]/);
		// The save handler derives its set from the checkbox draft, not a single select value.
		const fn = s.slice(s.indexOf('async function handleSaveGroups'), s.indexOf('// M20 W2:'));
		expect(fn).toMatch(/\[\.\.\.\(contactGroupDraft\[hb_id\] \?\? \[\]\)\]/);
		expect(fn).toMatch(/contactUpdateGroups\(hb_id, groupNames\)/);
	});
});
